//! The checks a hardware backend runs on the device on an indirect command's
//! arguments, between the command that last wrote them and the command that
//! reads them (`plans/GPU.md` GP5.4): what the software device judges as the
//! command runs, judged where the arguments lie.

use tairix_spirv::{Admitted, BuildError, Op};

use crate::kit::Pass;

/// An indirect dispatch's counts: each axis held to the device's, every axis
/// zeroed and the slot's fault set where any is past it, so the dispatch
/// runs nothing and its context is lost.
pub mod counts {
    use super::{Admitted, BuildError, Op, Pass};

    /// The module as committed.
    pub const SPIRV: &[u8] = include_bytes!("../spv/indirect_counts.spv");

    /// The binding of the counts checked: a slot four words long each, its
    /// first three the counts along each axis.
    pub const COUNTS: u32 = 0;

    /// The binding of the faults: a word a slot, set to one where its
    /// counts were past the device's and left as it was otherwise.
    pub const FAULTS: u32 = 1;

    /// The words a slot of counts takes.
    pub const SLOT_WORDS: u32 = 4;

    const SLOT: u32 = 0;
    const MOST: u32 = 1;
    /// The push constants' words.
    pub const WORDS: usize = 4;

    /// What a check is given.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub struct Push {
        /// The slot of the counts checked and of their fault.
        pub slot: u32,
        /// The most workgroups the device makes along each axis.
        pub most: [u32; 3],
    }

    impl Push {
        /// The push constants, in the module's order.
        #[must_use]
        pub const fn words(&self) -> [u32; WORDS] {
            [self.slot, self.most[0], self.most[1], self.most[2]]
        }
    }

    /// The module, as its source writes it.
    ///
    /// # Errors
    ///
    /// The builder's refusal.
    pub fn build() -> Result<Admitted, BuildError> {
        let mut pass = Pass::new(WORDS)?;
        let counts = pass.buffer(COUNTS)?;
        let faults = pass.buffer(FAULTS)?;
        pass.finish_once(|f, kit| {
            let slot = kit.field(f, SLOT)?;
            let first = kit.op_by(f, Op::I_MUL, slot, SLOT_WORDS)?;
            let mut places = [first; 3];
            let mut values = [first; 3];
            let mut exceeded = f.module().constant_bool(kit.boolean, false)?;
            for (axis, (at, value)) in (0u32..).zip(places.iter_mut().zip(&mut values)) {
                *at = kit.op_by(f, Op::I_ADD, first, axis)?;
                *value = kit.load(f, counts, *at)?;
                let most = kit.field(f, MOST + axis)?;
                let over = kit.test(f, Op::U_GREATER_THAN, *value, most)?;
                exceeded = f.op(Op::LOGICAL_OR, kit.boolean, &[exceeded, over])?;
            }
            let zero = f.module().uint(0)?;
            for (at, value) in places.into_iter().zip(values) {
                let kept = kit.select(f, exceeded, zero, value)?;
                kit.store(f, counts, at, kept)?;
            }
            let one = f.module().uint(1)?;
            let fault = kit.load(f, faults, slot)?;
            let marked = kit.select(f, exceeded, one, fault)?;
            kit.store(f, faults, slot, marked)
        })
    }
}

/// An indirect draw's records, copied one after another into its slot's
/// records: each naming a first instance other than nought is zeroed and the
/// slot's fault set, so the draw draws nothing of it and its context is lost.
pub mod records {
    use super::{Admitted, BuildError, Op, Pass};

    /// The module as committed.
    pub const SPIRV: &[u8] = include_bytes!("../spv/indirect_records.spv");

    /// The binding of the records checked, one after another.
    pub const RECORDS: u32 = 0;

    /// The binding of the faults: a word a slot, set to one where a record
    /// named a first instance and left as it was otherwise.
    pub const FAULTS: u32 = 1;

    const SLOT: u32 = 0;
    const FIRST: u32 = 1;
    const COUNT: u32 = 2;
    const RECORD_WORDS: u32 = 3;
    /// The push constants' words.
    pub const WORDS: usize = 4;

    /// What a check is given.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub struct Push {
        /// The slot of the fault set.
        pub slot: u32,
        /// The word the first record starts at.
        pub first: u32,
        /// How many records there are.
        pub count: u32,
        /// Whether they are an indexed draw's.
        pub indexed: bool,
    }

    impl Push {
        /// The push constants, in the module's order.
        #[must_use]
        pub const fn words(&self) -> [u32; WORDS] {
            // A record's last word is its first instance.
            let record = if self.indexed { 5 } else { 4 };
            [self.slot, self.first, self.count, record]
        }
    }

    /// The module, as its source writes it.
    ///
    /// # Errors
    ///
    /// The builder's refusal.
    pub fn build() -> Result<Admitted, BuildError> {
        let mut pass = Pass::new(WORDS)?;
        let records = pass.buffer(RECORDS)?;
        let faults = pass.buffer(FAULTS)?;
        pass.finish_once(|f, kit| {
            let slot = kit.field(f, SLOT)?;
            let first = kit.field(f, FIRST)?;
            let count = kit.field(f, COUNT)?;
            let words = kit.field(f, RECORD_WORDS)?;
            let (zero, one) = (f.module().uint(0)?, f.module().uint(1)?);
            let seen = kit.local(f, zero)?;
            kit.repeat(f, count, |f, record| {
                let into = kit.op(f, Op::I_MUL, record, words)?;
                let start = kit.op(f, Op::I_ADD, first, into)?;
                let end = kit.op(f, Op::I_ADD, start, words)?;
                let last = kit.op_by(f, Op::I_SUB, end, 1)?;
                let instance = kit.load(f, records, last)?;
                let names = kit.test(f, Op::I_NOT_EQUAL, instance, zero)?;
                f.if_then(names, |f| {
                    kit.repeat(f, words, |f, word| {
                        let at = kit.op(f, Op::I_ADD, start, word)?;
                        kit.store(f, records, at, zero)
                    })?;
                    f.emit(Op::STORE, &[seen, one])
                })
            })?;
            let any = f.op(Op::LOAD, kit.uint, &[seen])?;
            let faulted = kit.test(f, Op::I_NOT_EQUAL, any, zero)?;
            let fault = kit.load(f, faults, slot)?;
            let marked = kit.select(f, faulted, one, fault)?;
            kit.store(f, faults, slot, marked)
        })
    }
}
