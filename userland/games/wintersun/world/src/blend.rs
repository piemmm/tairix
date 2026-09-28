//! A cell's normalised weights over a closed vocabulary.
//!
//! A cell does not have *a* biome or *a* ground. It has a weight vector over
//! the vocabulary, kept to the widest blend the splat renderer can draw in
//! one pass, with integer weights summing to exactly [`WEIGHT_TOTAL`] — so
//! "the weights are normalised" is a property of the type rather than a
//! convention a consumer has to trust. Where the conditions change gradually
//! — from one climate into the next — the boundary between two biomes, or
//! two grounds, is therefore a gradient rather than a line something has to
//! hide.
//!
//! Biomes and grounds are weighed the same way, so the normalisation is
//! written once, generic over the [`Kind`] it weighs.

use tairix_util::mathf;

use crate::geom::quantise_u8;

/// Weights in a blend, summing to [`WEIGHT_TOTAL`].
///
/// Four is what the splat pass can take in one go, and more than four kinds
/// meeting at one cell is a boundary of boundaries no renderer would resolve
/// anyway.
pub const BLEND_SLOTS: usize = 4;

/// What a blend's weights sum to, always.
pub const WEIGHT_TOTAL: u16 = 255;

/// A closed vocabulary a [`Blend`] weighs.
pub trait Kind: Copy + Eq + 'static {
    /// Every member, in identifier order.
    const ALL: &'static [Self];

    /// The member's position in [`Kind::ALL`]: the identifier a stored world
    /// edit or a digest carries.
    fn id(self) -> u8;
}

/// A cell's kinds and their weights, heaviest first.
///
/// Weights sum to [`WEIGHT_TOTAL`]. A slot with zero weight is unused and
/// holds the heaviest kind, so two blends that weigh the same kinds the same
/// are equal.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Blend<K> {
    kinds: [K; BLEND_SLOTS],
    weights: [u8; BLEND_SLOTS],
}

impl<K: Kind> Blend<K> {
    /// A blend of one kind.
    #[must_use]
    pub fn solid(kind: K) -> Self {
        let mut weights = [0; BLEND_SLOTS];
        #[allow(
            clippy::cast_possible_truncation,
            reason = "WEIGHT_TOTAL is 255, which is a u8"
        )]
        {
            weights[0] = WEIGHT_TOTAL as u8;
        }
        Self {
            kinds: [kind; BLEND_SLOTS],
            weights,
        }
    }

    /// The kinds, heaviest first.
    #[must_use]
    pub const fn kinds(&self) -> &[K; BLEND_SLOTS] {
        &self.kinds
    }

    /// Their weights, in the same order.
    #[must_use]
    pub const fn weights(&self) -> &[u8; BLEND_SLOTS] {
        &self.weights
    }

    /// The kinds that carry weight, with their weights, heaviest first.
    pub fn slots(&self) -> impl Iterator<Item = (K, u8)> + '_ {
        self.kinds
            .iter()
            .copied()
            .zip(self.weights.iter().copied())
            .filter(|&(_, weight)| weight > 0)
    }

    /// The heaviest kind — the label, for a consumer that wants one.
    #[must_use]
    pub const fn dominant(&self) -> K {
        self.kinds[0]
    }

    /// The weight `kind` carries here, zero where it is absent. Only the
    /// crate's tests ask one kind's share; generation reads whole slots.
    #[cfg(test)]
    pub(crate) fn weight_of(&self, kind: K) -> u8 {
        self.slots()
            .find(|&(held, _)| held == kind)
            .map_or(0, |(_, weight)| weight)
    }

    /// The weights' sum, which is always [`WEIGHT_TOTAL`].
    #[must_use]
    pub fn total(&self) -> u16 {
        self.weights.iter().map(|&w| u16::from(w)).sum()
    }

    /// Take the heaviest [`BLEND_SLOTS`] of `raw` — one weight per member of
    /// [`Kind::ALL`], in its order — and normalise them to [`WEIGHT_TOTAL`].
    ///
    /// `bare` answers a cell nothing claimed, which keeps the sum exact
    /// rather than leaving an unnormalised blend.
    #[must_use]
    pub fn normalise(raw: &[f64], bare: K) -> Self {
        // One pass, keeping the heaviest in order: a weight moves ahead only
        // of those strictly lighter, and weights arrive in identifier order,
        // so a tie resolves to the lower identifier everywhere.
        let mut chosen = [(0.0_f64, bare); BLEND_SLOTS];
        for (&weight, &kind) in raw.iter().zip(K::ALL) {
            if weight > chosen[BLEND_SLOTS - 1].0 {
                let mut at = BLEND_SLOTS - 1;
                while at > 0 && weight > chosen[at - 1].0 {
                    chosen[at] = chosen[at - 1];
                    at -= 1;
                }
                chosen[at] = (weight, kind);
            }
        }

        let total: f64 = chosen.iter().map(|&(weight, _)| weight).sum();
        if total <= 0.0 {
            return Self::solid(bare);
        }

        // Largest remainder, so the integer weights sum to exactly the total
        // rather than to whatever rounding each slot happened to give.
        let mut weights = [0_u16; BLEND_SLOTS];
        let mut remainders = [(0_i64, 0_usize); BLEND_SLOTS];
        let mut assigned = 0_u16;
        for (slot, &(weight, _)) in chosen.iter().enumerate() {
            let exact = weight / total * f64::from(WEIGHT_TOTAL);
            let floor = mathf::floor(exact);
            weights[slot] = u16::from(quantise_u8(floor));
            assigned += weights[slot];
            let remainder = i64::from(mathf::round_i32((exact - floor) * 1_048_576.0));
            remainders[slot] = (-remainder, slot);
        }
        remainders.sort_unstable();
        let mut cursor = 0;
        while assigned < WEIGHT_TOTAL {
            let (_, slot) = remainders[cursor % BLEND_SLOTS];
            weights[slot] += 1;
            assigned += 1;
            cursor += 1;
        }

        // The remainder pass can hand a +1 to a slot whose floor tied with
        // the one above it, so the heaviest-first order is restored here
        // rather than assumed, the original slot breaking a tie.
        let mut ordered = [(0_u16, 0_usize); BLEND_SLOTS];
        for (slot, weight) in weights.iter().copied().enumerate() {
            ordered[slot] = (u16::MAX - weight, slot);
        }
        ordered.sort_unstable();

        let mut kinds = [bare; BLEND_SLOTS];
        let mut out = [0_u8; BLEND_SLOTS];
        for (rank, &(_, slot)) in ordered.iter().enumerate() {
            kinds[rank] = chosen[slot].1;
            #[allow(
                clippy::cast_possible_truncation,
                reason = "the weights sum to WEIGHT_TOTAL, so none exceeds 255"
            )]
            {
                out[rank] = weights[slot] as u8;
            }
        }
        let heaviest = kinds[0];
        for (kind, &weight) in kinds.iter_mut().zip(&out) {
            if weight == 0 {
                *kind = heaviest;
            }
        }
        Self {
            kinds,
            weights: out,
        }
    }
}

#[cfg(test)]
mod tests;
