//! The stencil stage: what a face's test compares and what each outcome does
//! to the value held.
//!
//! The test compares the reference with the value held, each through the
//! compare mask; an outcome's operation then changes only the bits the write
//! mask names. Every stencil format holds eight bits.

use crate::depth::CompareOp;

/// What an outcome of the stencil and depth tests does to the stencil held.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum StencilOp {
    /// Leaves it.
    Keep,
    /// Clears it.
    Zero,
    /// Sets it to the reference.
    Replace,
    /// Adds one, stopping at the most it holds.
    IncrementAndClamp,
    /// Takes one away, stopping at nought.
    DecrementAndClamp,
    /// Inverts every bit.
    Invert,
    /// Adds one, wrapping past the most to nought.
    IncrementAndWrap,
    /// Takes one away, wrapping past nought to the most.
    DecrementAndWrap,
}

/// The stencil state of one face: front or back.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct StencilFace {
    /// Where the stencil test fails.
    pub fail: StencilOp,
    /// Where both the stencil and the depth tests pass.
    pub pass: StencilOp,
    /// Where the stencil test passes and the depth test fails.
    pub depth_fail: StencilOp,
    /// How the reference is compared with the value held.
    pub compare: CompareOp,
    /// The bits the comparison reads.
    pub compare_mask: u8,
    /// The bits an operation may change.
    pub write_mask: u8,
    /// The value compared and replaced with.
    pub reference: u8,
}

/// Where a fragment's tests left it, for the stencil it then writes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// The stencil test failed.
    StencilFailed,
    /// The stencil test passed and the depth test failed.
    DepthFailed,
    /// Both passed.
    Passed,
}

impl StencilFace {
    /// Every test passing and nothing written: a face with no stencil test.
    pub const KEEP: Self = Self {
        fail: StencilOp::Keep,
        pass: StencilOp::Keep,
        depth_fail: StencilOp::Keep,
        compare: CompareOp::Always,
        compare_mask: u8::MAX,
        write_mask: u8::MAX,
        reference: 0,
    };

    /// Whether the stencil test passes over the value `held`.
    #[must_use]
    pub fn passes(&self, held: u8) -> bool {
        self.compare
            .passes(self.reference & self.compare_mask, held & self.compare_mask)
    }

    /// The value held after a fragment ending at `outcome` writes over
    /// `held`: only the write mask's bits change.
    #[must_use]
    pub fn write(&self, outcome: Outcome, held: u8) -> u8 {
        let op = match outcome {
            Outcome::StencilFailed => self.fail,
            Outcome::DepthFailed => self.depth_fail,
            Outcome::Passed => self.pass,
        };
        let changed = match op {
            StencilOp::Keep => held,
            StencilOp::Zero => 0,
            StencilOp::Replace => self.reference,
            StencilOp::IncrementAndClamp => held.saturating_add(1),
            StencilOp::DecrementAndClamp => held.saturating_sub(1),
            StencilOp::Invert => !held,
            StencilOp::IncrementAndWrap => held.wrapping_add(1),
            StencilOp::DecrementAndWrap => held.wrapping_sub(1),
        };
        (held & !self.write_mask) | (changed & self.write_mask)
    }
}

#[cfg(test)]
#[path = "stencil_tests.rs"]
mod tests;
