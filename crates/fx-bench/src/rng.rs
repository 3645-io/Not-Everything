//! Deterministic pseudo-random numbers: xoshiro256** seeded through SplitMix64.
//!
//! Both algorithms are public domain (Blackman and Vigna) and are written
//! here from their published descriptions. Every operation is integer-only
//! wrapping arithmetic, so a seed yields the same stream bit for bit on every
//! OS, architecture and toolchain. No floating point is used anywhere in a
//! code path that affects output.

/// SplitMix64 increment (the 64-bit golden ratio).
const SPLITMIX_GAMMA: u64 = 0x9e37_79b9_7f4a_7c15;

/// One SplitMix64 step: advances `state` and returns the next output.
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(SPLITMIX_GAMMA);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// xoshiro256** generator.
///
/// The four state words are the first four SplitMix64 outputs for the seed,
/// which can never all be zero. Changing this stream for an existing seed is
/// a breaking change: the generator's golden checksums depend on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rng {
    s: [u64; 4],
}

impl Rng {
    /// Creates a generator from a 64-bit seed.
    pub fn new(seed: u64) -> Self {
        let mut sm = seed;
        let s = [
            splitmix64(&mut sm),
            splitmix64(&mut sm),
            splitmix64(&mut sm),
            splitmix64(&mut sm),
        ];
        Self { s }
    }

    /// Returns the next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        let s = &mut self.s;
        let result = s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = s[1] << 17;
        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];
        s[2] ^= t;
        s[3] = s[3].rotate_left(45);
        result
    }

    /// Returns a uniformly distributed value in `0..n`, without bias.
    ///
    /// Uses Lemire's multiply-and-reject method: the high half of a 128-bit
    /// product, rejecting the few low halves that would bias the result.
    ///
    /// # Panics
    ///
    /// Panics if `n` is zero.
    pub fn below(&mut self, n: u64) -> u64 {
        assert!(n > 0, "Rng::below(0) has no valid result");
        let mut m = u128::from(self.next_u64()) * u128::from(n);
        let mut low = m as u64;
        if low < n {
            let threshold = n.wrapping_neg() % n;
            while low < threshold {
                m = u128::from(self.next_u64()) * u128::from(n);
                low = m as u64;
            }
        }
        (m >> 64) as u64
    }

    /// Returns `true` with probability `p_per_million / 1_000_000`.
    ///
    /// Always consumes randomness, so the stream position does not depend on
    /// `p_per_million`. Values of one million or more always return `true`.
    pub fn chance(&mut self, p_per_million: u32) -> bool {
        self.below(1_000_000) < u64::from(p_per_million)
    }

    /// Returns a uniformly chosen element of a non-empty slice.
    ///
    /// # Panics
    ///
    /// Panics if `items` is empty.
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.index(items.len())]
    }

    /// Returns a uniformly distributed index in `0..len`.
    ///
    /// # Panics
    ///
    /// Panics if `len` is zero.
    pub fn index(&mut self, len: usize) -> usize {
        self.below(len as u64) as usize
    }

    /// Picks an element of a weighted table: each item is chosen with
    /// probability `weight / sum of weights`.
    ///
    /// # Panics
    ///
    /// Panics if the table is empty or all weights are zero.
    pub fn weighted<'a, T>(&mut self, table: &'a [(T, u32)]) -> &'a T {
        let total: u64 = table.iter().map(|(_, w)| u64::from(*w)).sum();
        let mut r = self.below(total);
        for (item, w) in table {
            let w = u64::from(*w);
            if r < w {
                return item;
            }
            r -= w;
        }
        unreachable!("weighted pick fell off the end of the table")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weighted_respects_zero_weights() {
        let mut rng = Rng::new(1);
        let table = [("a", 0u32), ("b", 3), ("c", 0)];
        for _ in 0..1_000 {
            assert_eq!(*rng.weighted(&table), "b");
        }
    }

    #[test]
    fn pick_and_index_stay_in_range() {
        let mut rng = Rng::new(2);
        let items = [1, 2, 3];
        for _ in 0..1_000 {
            assert!(items.contains(rng.pick(&items)));
            assert!(rng.index(5) < 5);
        }
    }
}
