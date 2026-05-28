//! Injectable RNG used by Display ID seeding and Mutation IDs.
//!
//! `tk init` does not consume the RNG — the slice creates `schema_migrations`
//! and `store_config` but allocates no display IDs. The trait + env-var
//! contract are still ratified here so downstream slices (`tk add`, etc.)
//! don't have to relitigate the seam.
//!
//! Determinism seam: `TK_RAND_SEED=<u64>` overrides the OS RNG with a
//! deterministic ChaCha-style stream. The mechanism is intentionally not
//! exercised by `tk init`'s scenario; it is exercised when the first
//! consuming command's slice lands.

/// Common RNG seam.
pub trait Rng {
    /// Draw 64 bits of randomness. Implementations are free to source these
    /// from the OS, a seeded stream, or a fixed-value fake.
    fn next_u64(&self) -> u64;
}

/// Production RNG. Reads `TK_RAND_SEED` at construction time; if set, every
/// `next_u64` call advances a deterministic SplitMix64 stream instead of
/// reading the OS RNG. SplitMix64 is the textbook seeded PRNG — small,
/// `unsafe`-free, and well-understood; we'll swap it for a richer generator
/// only when the first downstream slice exercises this seam under pressure.
///
/// Modelled as a 2-variant enum so the mutable `Cell` only lives on the
/// `Seeded` arm — the OS arm cannot accidentally read or write per-call state.
pub enum RealRng {
    Os,
    Seeded(std::cell::Cell<u64>),
}

impl RealRng {
    #[must_use]
    pub fn new() -> Self {
        read_seed_override().map_or(RealRng::Os, Self::seeded)
    }

    /// Construct a deterministic RNG seeded with `seed`. Public so
    /// command-handler tests can avoid env-var manipulation.
    #[must_use]
    pub fn seeded(seed: u64) -> Self {
        RealRng::Seeded(std::cell::Cell::new(seed))
    }
}

fn read_seed_override() -> Option<u64> {
    std::env::var("TK_RAND_SEED")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
}

impl Default for RealRng {
    fn default() -> Self {
        Self::new()
    }
}

impl Rng for RealRng {
    fn next_u64(&self) -> u64 {
        match self {
            RealRng::Os => os_rand_u64(),
            RealRng::Seeded(cell) => {
                let mut state = cell.get().wrapping_add(0x9E37_79B9_7F4A_7C15);
                cell.set(state);
                state = (state ^ (state >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                state = (state ^ (state >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                state ^ (state >> 31)
            }
        }
    }
}

fn os_rand_u64() -> u64 {
    // Slice 0 keeps OS-RNG access dependency-free. `RandomState` is seeded
    // once per call from the OS RNG (libstd does this for HashMap DoS
    // resistance), and the hasher's finished state gives a u64 derived from
    // those keys. This is not a CSPRNG — the first downstream slice that
    // actually consumes the RNG should swap this for `getrandom`.
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_mix_is_deterministic_given_a_seed() {
        // The determinism seam is the entire point of this module. Two RNGs
        // seeded the same must produce the same draw sequence. The env-var
        // entry path is exercised by the scenario harness instead, to avoid
        // racy in-process `set_var` in tests.
        let a = RealRng::seeded(42);
        let b = RealRng::seeded(42);
        for _ in 0..8 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }
}
