//! The game's random number stream.
//!
//! `CUniformRandomStream` (`vstdlib/random.cpp:79`) — the `random->RandomInt`
//! and `random->RandomFloat` that `logic_case` and `logic_timer` call.
//!
//! # Why this is ported rather than taken from a crate
//!
//! `PORTING.md` says to prefer a crate over transliterating in-engine code, and
//! this is one of the cases where that rule points the other way. The generator
//! is *Numerical Recipes*' `ran1` — a Park-Miller multiplicative congruential
//! stream behind a Bays-Durham shuffle table — and it is sixty lines. What
//! would be lost by swapping it for `rand` is not speed or quality, which do
//! not matter for picking one of four `logic_case` outputs, but the two pieces
//! of behaviour a future reader would have to rediscover: **`RandomInt`
//! rejects rather than taking a modulus**, so its distribution has no bias at
//! the top of the range, and **the seed convention is inverted and lossy** —
//! `SetSeed` stores `-seed`, and the warm-up then collapses 0, 1 and -1 onto
//! one stream (see [`RandomStream::set_seed`]). Adding a dependency to get
//! *different* numbers is not a trade worth making.
//!
//! The stream is **not** a global. Valve's is a file-scope
//! `s_UniformStream` behind `InstallUniformRandomStream`; here it belongs to
//! the [`Server`](super::Server), which is what makes a test deterministic by
//! construction rather than by remembering to call `RandomSeed` first.

/// `IA` — Park-Miller's multiplier, 7^5.
const IA: i32 = 16807;
/// `IM` — the modulus, 2^31 - 1.
const IM: i32 = 2_147_483_647;
/// `IQ` — `IM / IA`, for Schrage's overflow-free multiplication.
const IQ: i32 = 127_773;
/// `IR` — `IM % IA`.
const IR: i32 = 2836;
/// `NTAB` (`public/vstdlib/random.h:20`) — the shuffle table's size.
const NTAB: usize = 32;
/// `NDIV` — how wide a slice of the output range each table slot covers.
const NDIV: i32 = 1 + (IM - 1) / NTAB as i32;
/// `AM` — `1 / IM`, the scale to `[0, 1)`.
const AM: f32 = 1.0 / IM as f32;
/// `RNMX` — `1 - EPS`, the largest float the stream may return, so that
/// `RandomFloat` is half-open at the top.
const RNMX: f32 = 1.0 - 1.2e-7;
/// `MAX_RANDOM_RANGE`.
const MAX_RANDOM_RANGE: u32 = 0x7FFF_FFFF;

/// `CUniformRandomStream`.
pub struct RandomStream {
    /// `m_idum` — the generator state. Negative means "reseed on next use",
    /// which is the convention [`RandomStream::set_seed`] relies on.
    idum: i32,
    /// `m_iy` — the last value drawn from the shuffle table. Zero also forces
    /// a reseed.
    iy: i32,
    /// `m_iv` — the shuffle table.
    iv: [i32; NTAB],
}

impl RandomStream {
    /// A stream seeded with `seed`. Valve's default construction is
    /// `SetSeed(0)`.
    pub fn new(seed: i32) -> RandomStream {
        let mut stream = RandomStream {
            idum: 0,
            iy: 0,
            iv: [0; NTAB],
        };
        stream.set_seed(seed);
        stream
    }

    /// `CUniformRandomStream::SetSeed` (`random.cpp:85`).
    ///
    /// **Stores the negated seed.** `m_idum = (iSeed < 0) ? iSeed : -iSeed`,
    /// so the state is always `<= 0` after seeding and the first
    /// [`next`](RandomStream::next) runs the table warm-up. A port that stored
    /// the seed as given would produce a different stream and would look
    /// correct.
    ///
    /// > **Seeds 0, 1 and -1 are the same stream.** The warm-up in
    /// > [`next`](RandomStream::next) opens with
    /// > `if ( -(m_idum) < 1 ) m_idum = 1; else m_idum = -(m_idum);`, so a
    /// > stored `0` and a stored `-1` both become 1 — and `SetSeed(1)` stores
    /// > `-1`. It is `ran1`'s "be sure to prevent idum = 0" guard swallowing
    /// > one seed with it. Valve seeds from the wall clock and never notices;
    /// > a test that seeds 0 and 1 and expects two streams gets one.
    pub fn set_seed(&mut self, seed: i32) {
        self.idum = match seed < 0 {
            true => seed,
            // `-i32::MIN` overflows; Valve's `-iSeed` is undefined there and
            // wraps to `i32::MIN` on every compiler this ever ran on.
            false => seed.wrapping_neg(),
        };
        self.iy = 0;
    }

    /// `CUniformRandomStream::GenerateRandomNumber` (`random.cpp:92`) — one
    /// draw in `[0, IM)`.
    ///
    /// Schrage's method (`k = idum / IQ; idum = IA * (idum - k * IQ) - IR * k`)
    /// keeps the multiply inside 32 bits, which is why the constants are what
    /// they are. The first call discards `NTAB + 8` draws while filling the
    /// shuffle table, exactly as `ran1` prescribes.
    pub fn next(&mut self) -> i32 {
        if self.idum <= 0 || self.iy == 0 {
            self.idum = match -self.idum < 1 {
                true => 1,
                false => -self.idum,
            };
            for j in (0..NTAB + 8).rev() {
                let k = self.idum / IQ;
                self.idum = IA * (self.idum - k * IQ) - IR * k;
                if self.idum < 0 {
                    self.idum += IM;
                }
                if j < NTAB {
                    self.iv[j] = self.idum;
                }
            }
            self.iy = self.iv[0];
        }

        let k = self.idum / IQ;
        self.idum = IA * (self.idum - k * IQ) - IR * k;
        if self.idum < 0 {
            self.idum += IM;
        }
        // Valve bounds-checks this index and warns, because it once saw memory
        // corruption here; `NTAB` is a power of two and `iy` is in `[0, IM)`,
        // so the quotient cannot leave the table and the array access proves it.
        let j = (self.iy / NDIV) as usize;
        self.iy = self.iv[j];
        self.iv[j] = self.idum;
        self.iy
    }

    /// `CUniformRandomStream::RandomFloat` (`random.cpp:141`) — uniform in
    /// `[low, high)`.
    pub fn float(&mut self, low: f32, high: f32) -> f32 {
        let unit = (AM * self.next() as f32).min(RNMX);
        unit * (high - low) + low
    }

    /// `CUniformRandomStream::RandomInt` (`random.cpp:167`) — uniform in
    /// `[low, high]`, **inclusive at both ends**.
    ///
    /// The loop is Valve's and is the interesting part: taking `next() % x`
    /// would over-represent the low end of the range whenever `x` does not
    /// divide `IM`, so draws above the largest exact multiple are **rejected
    /// and redrawn**. Even for the worst `x` the loop is taken at most half
    /// the time, so it terminates in two draws on average.
    ///
    /// A range of one value or fewer returns `low` without drawing at all,
    /// which matters: it means an empty `logic_case` does not disturb the
    /// stream.
    pub fn int(&mut self, low: i32, high: i32) -> i32 {
        let span = (high as i64) - (low as i64) + 1;
        if span <= 1 || span - 1 > MAX_RANDOM_RANGE as i64 {
            return low;
        }
        let span = span as u32;
        let max_acceptable =
            MAX_RANDOM_RANGE - ((MAX_RANDOM_RANGE as u64 + 1) % span as u64) as u32;
        loop {
            let n = self.next() as u32;
            if n <= max_acceptable {
                return low.wrapping_add((n % span) as i32);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stream is a pure function of its seed, which is what every test
    /// that pokes `logic_case` depends on.
    #[test]
    fn the_same_seed_gives_the_same_stream() {
        let draw = |seed| {
            let mut stream = RandomStream::new(seed);
            (0..8).map(|_| stream.next()).collect::<Vec<_>>()
        };
        assert_eq!(draw(7), draw(7));
        assert_ne!(draw(7), draw(8));

        // `ran1`'s zero guard: 0, 1 and -1 all warm up to the same state, so
        // the three are one stream. See [`RandomStream::set_seed`].
        assert_eq!(draw(0), draw(1));
        assert_eq!(draw(0), draw(-1));
        assert_ne!(draw(0), draw(2));
    }

    /// `ran1`'s output range, and the half-open float.
    #[test]
    fn draws_stay_inside_their_range() {
        let mut stream = RandomStream::new(1);
        for _ in 0..2000 {
            let n = stream.next();
            assert!((1..IM).contains(&n), "{n} is outside [1, IM)");

            let f = stream.float(2.0, 5.0);
            assert!((2.0..5.0).contains(&f), "{f} is outside [2, 5)");

            let i = stream.int(3, 7);
            assert!((3..=7).contains(&i), "{i} is outside [3, 7]");
        }
    }

    /// Both ends of `RandomInt`'s range are reachable — the off-by-one a
    /// half-open port would introduce.
    #[test]
    fn random_int_is_inclusive_at_both_ends() {
        let mut stream = RandomStream::new(7);
        let mut seen = [false; 4];
        for _ in 0..500 {
            seen[stream.int(0, 3) as usize] = true;
        }
        assert_eq!(seen, [true; 4]);
    }

    /// A degenerate range returns `low` and **does not draw**, so it cannot
    /// shift the stream.
    #[test]
    fn a_range_of_one_does_not_disturb_the_stream() {
        let mut stream = RandomStream::new(3);
        let expected = RandomStream::new(3).next();
        assert_eq!(stream.int(5, 5), 5);
        assert_eq!(stream.int(9, 2), 9, "an inverted range is also degenerate");
        assert_eq!(stream.next(), expected);
    }

    /// The distribution is at least not obviously broken: 16 buckets over
    /// 16,000 draws should each land within a factor of two of 1,000.
    #[test]
    fn the_distribution_is_roughly_uniform() {
        let mut stream = RandomStream::new(1);
        let mut buckets = [0u32; 16];
        for _ in 0..16_000 {
            buckets[stream.int(0, 15) as usize] += 1;
        }
        for (i, count) in buckets.iter().enumerate() {
            assert!(
                (500..2000).contains(count),
                "bucket {i} got {count} of 16000"
            );
        }
    }
}
