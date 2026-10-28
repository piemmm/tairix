//! Subgroup operations: each over the `Subgroup` execution scope, its value
//! and result types — Booleans, 32-bit integers and 32- or 64-bit floats —
//! and the group operations a reduction admits.

use super::body::{Args, Reduce, Typing};
use super::types::{Scalar, Ty};
use super::{Rule, Validator, What};
use crate::enumerant::{GroupOperation, Scope};

impl Validator<'_> {
    /// Check one subgroup operation.
    #[expect(clippy::too_many_lines, reason = "one arm per subgroup operation")]
    pub(crate) fn group(&self, typing: Typing, args: &Args<'_>) -> Result<(), Rule> {
        let r = args.result_type;
        let scope = self.arg_id(args, 0)?;
        if self.constant_word(scope).map(Scope) != Some(Scope::SUBGROUP) {
            return Err(Rule::Semantics);
        }
        let bool_scalar = |ty: u32| matches!(self.ty(ty).ty, Ty::Scalar(Scalar::Bool));
        let unsigned_scalar = |ty: u32| matches!(self.ty(ty).ty, Ty::Scalar(s) if s.is_unsigned());
        let ballot = |ty: u32| {
            matches!(
                self.ty(ty).ty,
                Ty::Vector {
                    scalar: Scalar::Int {
                        width: 32,
                        signed: false
                    },
                    count: 4
                }
            )
        };
        let any_value = |ty: u32| self.components(ty).is_some_and(|(s, _)| portable(s));
        let arity = |n: usize| {
            if args.len() == n {
                Ok(())
            } else {
                Err(Rule::OperandType(args.offset))
            }
        };
        match typing {
            Typing::Elect => {
                arity(1)?;
                result(bool_scalar(r))
            }
            Typing::Vote => {
                arity(2)?;
                result(bool_scalar(r))?;
                operand(args, 1, bool_scalar(self.arg_type(args, 1)?))
            }
            Typing::AllEqual => {
                arity(2)?;
                result(bool_scalar(r))?;
                operand(args, 1, any_value(self.arg_type(args, 1)?))
            }
            Typing::Broadcast | Typing::BroadcastFirst => {
                result(any_value(r))?;
                operand(args, 1, self.arg_type(args, 1)? == r)?;
                if typing == Typing::Broadcast {
                    arity(3)?;
                    // A dynamic invocation id needs a device feature.
                    let id = self.arg_id(args, 2)?;
                    let constant = self.def(id)?.what == What::Constant;
                    operand(
                        args,
                        2,
                        unsigned_scalar(self.arg_type(args, 2)?) && constant,
                    )
                } else {
                    arity(2)
                }
            }
            Typing::Ballot => {
                arity(2)?;
                result(ballot(r))?;
                operand(args, 1, bool_scalar(self.arg_type(args, 1)?))
            }
            Typing::InverseBallot | Typing::BallotFind => {
                arity(2)?;
                result(if typing == Typing::InverseBallot {
                    bool_scalar(r)
                } else {
                    unsigned_scalar(r)
                })?;
                operand(args, 1, ballot(self.arg_type(args, 1)?))
            }
            Typing::BallotBitExtract => {
                arity(3)?;
                result(bool_scalar(r))?;
                operand(args, 1, ballot(self.arg_type(args, 1)?))?;
                operand(args, 2, unsigned_scalar(self.arg_type(args, 2)?))
            }
            Typing::BallotBitCount => {
                arity(3)?;
                result(unsigned_scalar(r))?;
                self.group_operation(args, 1)?;
                operand(args, 2, ballot(self.arg_type(args, 2)?))
            }
            Typing::GroupReduce(kind) => {
                arity(3)?;
                let pick: fn(Scalar) -> bool = match kind {
                    Reduce::Int => Scalar::is_int,
                    Reduce::Unsigned => Scalar::is_unsigned,
                    Reduce::Float => Scalar::is_float,
                    Reduce::Logical => Scalar::is_bool,
                };
                result(
                    self.components(r)
                        .is_some_and(|(s, _)| pick(s) && portable(s)),
                )?;
                self.group_operation(args, 1)?;
                operand(args, 2, self.arg_type(args, 2)? == r)
            }
            _ => Err(Rule::NotAdmitted(args.op)),
        }
    }

    /// A reduction or scan over the whole subgroup.
    fn group_operation(&self, args: &Args<'_>, n: usize) -> Result<(), Rule> {
        let operation = GroupOperation(self.arg_word(args, n)?);
        if matches!(
            operation,
            GroupOperation::REDUCE
                | GroupOperation::INCLUSIVE_SCAN
                | GroupOperation::EXCLUSIVE_SCAN
        ) {
            Ok(())
        } else {
            Err(Rule::OperandValue(
                args.offset
                    .saturating_add(u8::try_from(n).unwrap_or(u8::MAX)),
            ))
        }
    }
}

/// Whether every device takes `scalar` in a subgroup operation: other widths
/// need a device feature.
const fn portable(scalar: Scalar) -> bool {
    matches!(
        scalar,
        Scalar::Bool | Scalar::Int { width: 32, .. } | Scalar::Float { width: 32 | 64 }
    )
}

const fn result(ok: bool) -> Result<(), Rule> {
    if ok {
        Ok(())
    } else {
        Err(Rule::ResultType)
    }
}

fn operand(args: &Args<'_>, n: usize, ok: bool) -> Result<(), Rule> {
    if ok {
        Ok(())
    } else {
        Err(args.wrong(n))
    }
}
