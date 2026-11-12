//! Parallel moves: the assignment an edge makes to its block's parameters,
//! or a call to its argument registers, every source read before any
//! destination is written, made a sequence of single moves.

use alloc::vec::Vec;

use crate::regalloc::Loc;

/// One move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Move {
    /// Where the value is.
    pub from: Loc,
    /// Where it goes.
    pub to: Loc,
}

/// `moves`, every destination distinct, as a sequence that leaves each
/// destination holding what its source held before any was made. A cycle is
/// broken through `temp`, a register none of `moves` names.
#[must_use]
pub fn sequence(moves: &[Move], temp: Loc) -> Vec<Move> {
    let mut pending: Vec<Move> = moves.iter().copied().filter(|m| m.from != m.to).collect();
    let mut out = Vec::with_capacity(pending.len() + 1);
    while !pending.is_empty() {
        // A move whose destination no pending move still reads may go.
        let ready =
            (0..pending.len()).find(|&n| pending.iter().all(|other| other.from != pending[n].to));
        if let Some(n) = ready {
            out.push(pending.remove(n));
        } else {
            // Every destination is still read: the rest are cycles. Save one
            // destination's value, and read it from there instead.
            let held = pending[0].to;
            out.push(Move {
                from: held,
                to: temp,
            });
            for pending in &mut pending {
                if pending.from == held {
                    pending.from = temp;
                }
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "moves_tests.rs"]
mod tests;
