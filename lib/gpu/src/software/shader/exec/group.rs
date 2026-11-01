//! What lanes compute together: built-ins of where each lane is, subgroup
//! operations over a subgroup's active lanes in lane order, and derivatives
//! across a quad.

use tairix_inline::ArrayVec;

use super::{each_lane, Lanes, Machine};
use crate::software::shader::ir::{
    Builtin, Cmp, Derivative, Fold, Group, Num, Op2, Reg, Relation, Scan, LANES,
};
use crate::software::shader::ops;

impl Machine<'_, '_, '_> {
    /// Each lane's words of `which`, word by word.
    pub(super) fn builtin_values(&self, which: Builtin, out: &mut [Lanes; 4]) {
        let size = self.env.program.workgroup_size;
        let width = self.env.subgroup_size.max(1);
        let invocations = size.iter().fold(1u32, |n, &s| n.saturating_mul(s));
        let members = self.members();
        for lane in 0..LANES {
            let local = self.place.local.get(lane).copied().unwrap_or(0);
            let slot = self.place.group_slot.get(lane).copied().unwrap_or(0) as usize;
            let group = self.place.group_id.get(slot).copied().unwrap_or([0; 3]);
            let (first, _) = self.place.subgroup.get(lane).copied().unwrap_or((0, 0));
            let within = u32::try_from(lane)
                .unwrap_or(0)
                .saturating_sub(u32::from(first));
            let at = local_id(local, size);
            let words: [u64; 4] = match which {
                Builtin::LocalInvocationId => widen3(at),
                Builtin::GlobalInvocationId => widen3(core::array::from_fn(|axis| {
                    group[axis].wrapping_mul(size[axis]).wrapping_add(at[axis])
                })),
                Builtin::WorkgroupId => widen3(group),
                Builtin::NumWorkgroups => widen3(self.env.groups),
                Builtin::WorkgroupSize => widen3(size),
                Builtin::LocalInvocationIndex => [u64::from(local), 0, 0, 0],
                Builtin::SubgroupSize => [u64::from(width), 0, 0, 0],
                Builtin::SubgroupLocalInvocationId => [u64::from(within), 0, 0, 0],
                Builtin::NumSubgroups => [u64::from(invocations.div_ceil(width).max(1)), 0, 0, 0],
                Builtin::SubgroupId => [u64::from(local / width), 0, 0, 0],
                Builtin::SubgroupEqMask => bits128(1u128 << within),
                Builtin::SubgroupGeMask => bits128(u128::MAX << within & members),
                Builtin::SubgroupGtMask => bits128(u128::MAX << within << 1 & members),
                Builtin::SubgroupLeMask => bits128(!(u128::MAX << within << 1)),
                Builtin::SubgroupLtMask => bits128(!(u128::MAX << within)),
            };
            for (word, values) in out.iter_mut().enumerate() {
                if let (Some(slot), Some(&value)) = (values.get_mut(lane), words.get(word)) {
                    *slot = value;
                }
            }
        }
    }

    /// The bits of a ballot that name a subgroup's own invocations.
    fn members(&self) -> u128 {
        !(u128::MAX << self.env.subgroup_size.clamp(1, 64))
    }

    /// The first lane and the active lanes of each subgroup with a lane in
    /// focus, in turn.
    fn subgroups(&self) -> ArrayVec<(usize, u64), LANES> {
        let mut found = ArrayVec::new();
        let mut left = self.mask;
        while left != 0 {
            let lane = left.trailing_zeros() as usize;
            let (first, len) = self.place.subgroup.get(lane).copied().unwrap_or((0, 0));
            let lanes = span_bits(usize::from(first), usize::from(len).max(1)) & self.mask;
            left &= !(lanes | (1 << lane));
            if found
                .try_push((usize::from(first), lanes | (1 << lane)))
                .is_err()
            {
                break;
            }
        }
        found
    }

    #[expect(clippy::too_many_lines, reason = "one arm per subgroup operation")]
    pub(super) fn group(&mut self, op: Group, dst: Reg, (a, b): (Reg, Reg)) {
        match op {
            Group::Elect => {
                let mut out = [0; LANES];
                for (_, lanes) in self.subgroups() {
                    if let Some(slot) = out.get_mut(lanes.trailing_zeros() as usize) {
                        *slot = 1;
                    }
                }
                self.store(dst, 0, false, &out);
            }
            Group::All | Group::Any => {
                let mut x = [0; LANES];
                self.fetch(a, 0, false, &mut x);
                let mut out = [0; LANES];
                for (_, lanes) in self.subgroups() {
                    let held = each_lane(lanes)
                        .filter(|&l| x.get(l).copied().unwrap_or(0) & 1 != 0)
                        .count();
                    let total = lanes.count_ones() as usize;
                    let answer = u64::from(if op == Group::All {
                        held == total
                    } else {
                        held > 0
                    });
                    for lane in each_lane(lanes) {
                        if let Some(slot) = out.get_mut(lane) {
                            *slot = answer;
                        }
                    }
                }
                self.store(dst, 0, false, &out);
            }
            Group::AllEqual { num, count } => self.all_equal(num, count, dst, a),
            Group::Broadcast { words } => self.broadcast(words, dst, a, Some(b)),
            Group::BroadcastFirst { words } => self.broadcast(words, dst, a, None),
            Group::Ballot => {
                let mut x = [0; LANES];
                self.fetch(a, 0, false, &mut x);
                let mut out = [[0u64; LANES]; 4];
                for (first, lanes) in self.subgroups() {
                    let mut ballot = 0u128;
                    for lane in each_lane(lanes) {
                        if x.get(lane).copied().unwrap_or(0) & 1 != 0 {
                            ballot |= 1 << (lane - first);
                        }
                    }
                    let words = bits128(ballot);
                    for lane in each_lane(lanes) {
                        for (word, values) in out.iter_mut().enumerate() {
                            if let (Some(slot), Some(&w)) = (values.get_mut(lane), words.get(word))
                            {
                                *slot = w;
                            }
                        }
                    }
                }
                for (word, values) in (0u32..).zip(&out) {
                    self.store(dst, word, false, values);
                }
            }
            Group::InverseBallot | Group::BallotBitExtract => {
                let value = self.ballot_value(a);
                let members = self.members();
                let mut index = [0; LANES];
                if op == Group::BallotBitExtract {
                    self.fetch(b, 0, false, &mut index);
                }
                let mut out = [0; LANES];
                for (first, lanes) in self.subgroups() {
                    for lane in each_lane(lanes) {
                        let bit = if op == Group::InverseBallot {
                            (lane - first) as u64
                        } else {
                            index.get(lane).copied().unwrap_or(0) & 0xffff_ffff
                        };
                        let held = value.get(lane).copied().unwrap_or(0) & members;
                        if let Some(slot) = out.get_mut(lane) {
                            *slot = u64::from(bit < 128 && held >> bit & 1 != 0);
                        }
                    }
                }
                self.store(dst, 0, false, &out);
            }
            Group::BallotBitCount(scan) => {
                let value = self.ballot_value(a);
                let members = self.members();
                let mut out = [0; LANES];
                for (first, lanes) in self.subgroups() {
                    for lane in each_lane(lanes) {
                        let within = lane - first;
                        let keep = match scan {
                            Scan::Reduce => members,
                            Scan::Inclusive => !(u128::MAX << within << 1),
                            Scan::Exclusive => !(u128::MAX << within),
                        };
                        let held = value.get(lane).copied().unwrap_or(0) & keep;
                        if let Some(slot) = out.get_mut(lane) {
                            *slot = u64::from(held.count_ones());
                        }
                    }
                }
                self.store(dst, 0, false, &out);
            }
            Group::BallotFindLsb | Group::BallotFindMsb => {
                let value = self.ballot_value(a);
                let members = self.members();
                let mut out = [0; LANES];
                for lane in each_lane(self.mask) {
                    let held = value.get(lane).copied().unwrap_or(0) & members;
                    let found = if held == 0 {
                        u64::from(u32::MAX)
                    } else if op == Group::BallotFindLsb {
                        u64::from(held.trailing_zeros())
                    } else {
                        u64::from(held.ilog2())
                    };
                    if let Some(slot) = out.get_mut(lane) {
                        *slot = found;
                    }
                }
                self.store(dst, 0, false, &out);
            }
            Group::Fold {
                op: fold,
                num,
                count,
                scan,
            } => self.fold(fold, (num, count), scan, dst, a),
        }
    }

    /// Each lane's four-word ballot `a` as one value.
    fn ballot_value(&self, a: Reg) -> [u128; LANES] {
        let mut out = [0u128; LANES];
        let mut x = [0; LANES];
        for word in 0..4u32 {
            self.fetch(a, word, false, &mut x);
            for lane in self.span.clone() {
                if let (Some(slot), Some(&w)) = (out.get_mut(lane), x.get(lane)) {
                    *slot |= u128::from(w & 0xffff_ffff) << (32 * word);
                }
            }
        }
        out
    }

    fn all_equal(&mut self, num: Num, count: u32, dst: Reg, a: Reg) {
        let words = num.words();
        let float_equal = ops::comparer(Cmp::Float {
            relation: Relation::Equal,
            ordered: true,
        });
        let mut equal = [1u64; LANES];
        let mut x = [0; LANES];
        for component in 0..count {
            self.fetch(a, component * words, words == 2, &mut x);
            for (_, lanes) in self.subgroups() {
                let first = x.get(lanes.trailing_zeros() as usize).copied().unwrap_or(0);
                let same = each_lane(lanes).all(|lane| {
                    let value = x.get(lane).copied().unwrap_or(0);
                    if num.is_float() {
                        ops::one_lane(float_equal, num, value, first) != 0
                    } else {
                        value == first
                    }
                });
                if !same {
                    for lane in each_lane(lanes) {
                        if let Some(slot) = equal.get_mut(lane) {
                            *slot = 0;
                        }
                    }
                }
            }
        }
        self.store(dst, 0, false, &equal);
    }

    /// Every lane's copy of the value of the lane `index` names within its
    /// subgroup, or of its first active lane; zero where that lane is not
    /// active.
    fn broadcast(&mut self, words: u32, dst: Reg, a: Reg, index: Option<Reg>) {
        let mut ids = [0; LANES];
        if let Some(index) = index {
            self.fetch(index, 0, false, &mut ids);
        }
        let mut sources = [None; LANES];
        for (first, lanes) in self.subgroups() {
            for lane in each_lane(lanes) {
                let source = match index {
                    Some(_) => {
                        let id = ids.get(lane).copied().unwrap_or(0) & 0xffff_ffff;
                        usize::try_from(id)
                            .ok()
                            .and_then(|id| first.checked_add(id))
                            .filter(|&at| at < LANES && lanes >> at & 1 != 0)
                    }
                    None => Some(lanes.trailing_zeros() as usize),
                };
                if let Some(slot) = sources.get_mut(lane) {
                    *slot = source;
                }
            }
        }
        let mut x = [0; LANES];
        let mut out = [0; LANES];
        for word in 0..words {
            self.fetch(a, word, false, &mut x);
            for lane in self.span.clone() {
                let value = sources
                    .get(lane)
                    .copied()
                    .flatten()
                    .and_then(|source| x.get(source))
                    .copied()
                    .unwrap_or(0);
                if let Some(slot) = out.get_mut(lane) {
                    *slot = value;
                }
            }
            self.store(dst, word, false, &out);
        }
    }

    /// `op` folded over each subgroup's active lanes in lane order: all of
    /// them, or a scan up to or before each lane.
    fn fold(&mut self, op: Fold, (num, count): (Num, u32), scan: Scan, dst: Reg, a: Reg) {
        let words = num.words();
        let combine = ops::binary(combining(op));
        let mut x = [0; LANES];
        let mut out = [0; LANES];
        for component in 0..count {
            self.fetch(a, component * words, words == 2, &mut x);
            for (_, lanes) in self.subgroups() {
                let mut running = identity(op, num);
                for lane in each_lane(lanes) {
                    let value = x.get(lane).copied().unwrap_or(0);
                    let before = running;
                    running = ops::one_lane(combine, num, running, value);
                    if let Some(slot) = out.get_mut(lane) {
                        *slot = match scan {
                            Scan::Exclusive => before,
                            Scan::Inclusive | Scan::Reduce => running,
                        };
                    }
                }
                if scan == Scan::Reduce {
                    for lane in each_lane(lanes) {
                        if let Some(slot) = out.get_mut(lane) {
                            *slot = running;
                        }
                    }
                }
            }
            self.store(dst, component * words, words == 2, &out);
        }
    }

    /// The derivative of `a` across each lane's quad: its own row or column
    /// where fine, the quad's first where coarse; a neighbour not running
    /// lends the lane's own value.
    pub(super) fn derivative(
        &mut self,
        op: Derivative,
        fine: bool,
        (num, count): (Num, u32),
        dst: Reg,
        a: Reg,
    ) {
        let words = num.words();
        let mut x = [0; LANES];
        let mut out = [0; LANES];
        let mask = self.mask;
        let value_of = |values: &Lanes, lane: usize, fallback: usize| {
            let at = if mask >> lane & 1 != 0 {
                lane
            } else {
                fallback
            };
            ops::float(num, values.get(at).copied().unwrap_or(0))
        };
        for component in 0..count {
            self.fetch_quads(a, component * words, words == 2, &mut x);
            for lane in each_lane(mask) {
                let base = if fine { lane } else { lane & !3 };
                let dx = value_of(&x, base | 1, lane) - value_of(&x, base & !1, lane);
                let dy = value_of(&x, base | 2, lane) - value_of(&x, base & !2, lane);
                let result = match op {
                    Derivative::X => dx,
                    Derivative::Y => dy,
                    Derivative::Width => dx.abs() + dy.abs(),
                };
                if let Some(slot) = out.get_mut(lane) {
                    *slot = ops::unfloat(num, result);
                }
            }
            self.store(dst, component * words, words == 2, &out);
        }
    }

    /// [`Machine::fetch`] over the whole quads the lanes in focus are in.
    fn fetch_quads(&mut self, reg: Reg, word: u32, wide: bool, out: &mut Lanes) {
        let span = self.span.clone();
        self.span = (span.start & !3)..(span.end.next_multiple_of(4)).min(LANES);
        self.fetch(reg, word, wide, out);
        self.span = span;
    }
}

/// What `op` folds from: the value it leaves another unchanged.
fn identity(op: Fold, num: Num) -> u64 {
    let bits = num.bits();
    match op {
        Fold::IAdd
        | Fold::BitOr
        | Fold::BitXor
        | Fold::LogicalOr
        | Fold::LogicalXor
        | Fold::UMax => 0,
        Fold::IMul | Fold::LogicalAnd => 1,
        Fold::BitAnd | Fold::UMin => ops::mask(bits),
        Fold::SMin => ops::mask(bits) >> 1,
        Fold::SMax => (ops::mask(bits) >> 1) + 1,
        Fold::FAdd => ops::unfloat(num, 0.0),
        Fold::FMul => ops::unfloat(num, 1.0),
        Fold::FMin => ops::unfloat(num, f64::INFINITY),
        Fold::FMax => ops::unfloat(num, f64::NEG_INFINITY),
    }
}

/// The operation `op` folds with.
const fn combining(op: Fold) -> Op2 {
    match op {
        Fold::IAdd => Op2::IAdd,
        Fold::IMul => Op2::IMul,
        Fold::SMin => Op2::SMin,
        Fold::SMax => Op2::SMax,
        Fold::UMin => Op2::UMin,
        Fold::UMax => Op2::UMax,
        Fold::FAdd => Op2::FAdd,
        Fold::FMul => Op2::FMul,
        Fold::FMin => Op2::NMin,
        Fold::FMax => Op2::NMax,
        Fold::BitAnd => Op2::BitAnd,
        Fold::BitOr => Op2::BitOr,
        Fold::BitXor => Op2::BitXor,
        Fold::LogicalAnd => Op2::LogicalAnd,
        Fold::LogicalOr => Op2::LogicalOr,
        Fold::LogicalXor => Op2::LogicalNotEqual,
    }
}

/// The `len` lanes from lane `first`.
fn span_bits(first: usize, len: usize) -> u64 {
    if len >= 64 {
        u64::MAX << first
    } else {
        ((1u64 << len) - 1) << first
    }
}

/// An invocation's place in its workgroup along each axis.
fn local_id(index: u32, size: [u32; 3]) -> [u32; 3] {
    let x = size[0].max(1);
    let y = size[1].max(1);
    [index % x, (index / x) % y, index / (x * y).max(1)]
}

fn widen3(v: [u32; 3]) -> [u64; 4] {
    [u64::from(v[0]), u64::from(v[1]), u64::from(v[2]), 0]
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "a 128-bit mask is split into its four words"
)]
fn bits128(v: u128) -> [u64; 4] {
    core::array::from_fn(|word| u64::from((v >> (32 * word)) as u32))
}
