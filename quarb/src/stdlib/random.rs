//! The random generator of the language (spec: Random Values;
//! ruling 2026-11-15).
//!
//! Everything here is part of the language's promise: a run with a
//! seed answers the same on every machine, in the browser build and
//! in every later release, so a transcript that pins its seed
//! replays. Nothing is taken from a library, whose algorithm may
//! move between versions. A change to any constant or step below
//! changes the answers of seeded runs and needs a ruling.
//!
//! - A seed is 64 bits.
//! - **Derivation** is SplitMix64 (Steele, Lea, Flood 2014): the
//!   state advances by `0x9E3779B97F4A7C15` and the output is the
//!   state mixed by two xor-shift-multiply rounds
//!   (`0xBF58476D1CE4E5B9`, `0x94D049BB133111EB`) and a final
//!   xor-shift. [`derive`] gives a seed from a seed and a number:
//!   one SplitMix64 output of the seed, xored with the number, and
//!   one SplitMix64 output of that.
//! - **The stream of a command**: the run's seed derived by the
//!   command's place among the commands of the query that draw,
//!   from one, in the order they first draw (the top-level pipeline
//!   commands; a call inside an expression belongs to the command
//!   the expression is written in). A command that draws nothing
//!   takes no place, so a projection written before a sample does
//!   not move what the sample picks. A command with a seed of its
//!   own (`--seed=n`) uses that seed derived by the same place.
//! - **The stream of a thread** (a draw made once per thread,
//!   `random()`): the command's stream seed derived by the thread's
//!   position, from one. So a draw does not depend on the order
//!   threads are evaluated in.
//! - **Drawing** is xoshiro256** (Blackman, Vigna 2018), its four
//!   words of state the first four SplitMix64 outputs of the stream
//!   seed.
//! - **A float** in [0, 1) is the top 53 bits of one output times
//!   2^-53.
//! - **An integer below a span** is one output reduced modulo the
//!   span, an output at or past the largest multiple of the span
//!   that fits 2^64 drawn again (rejection), so every value is as
//!   likely as every other.
//! - **A shuffle** is Fisher-Yates from the end: for i from the
//!   last position down to the second, the item at i changes places
//!   with the item at a position drawn from 0 to i.
//! - **A sample** without replacement shuffles the positions and
//!   takes the first n of the shuffled order, answered in the
//!   stream's own order; with replacement it draws n positions, in
//!   the order drawn.

/// One SplitMix64 step: the state advanced, the output returned.
fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A seed from a seed and a number: the command's stream from the
/// run's seed and the command's place, a thread's from the
/// command's and the thread's position.
pub(crate) fn derive(seed: u64, by: u64) -> u64 {
    let mut state = seed;
    let mut state = splitmix(&mut state) ^ by;
    splitmix(&mut state)
}

/// One stream of draws: xoshiro256**.
pub(crate) struct Stream {
    s: [u64; 4],
}

impl Stream {
    /// The stream of a seed: its state the first four SplitMix64
    /// outputs of the seed.
    pub(crate) fn seeded(seed: u64) -> Stream {
        let mut state = seed;
        Stream {
            s: [
                splitmix(&mut state),
                splitmix(&mut state),
                splitmix(&mut state),
                splitmix(&mut state),
            ],
        }
    }

    /// The next 64 bits.
    pub(crate) fn next(&mut self) -> u64 {
        let s = &mut self.s;
        let out = s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = s[1] << 17;
        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];
        s[2] ^= t;
        s[3] = s[3].rotate_left(45);
        out
    }

    /// A float from 0 up to 1, 1 left out: the top 53 bits of one
    /// output times 2^-53.
    pub(crate) fn float(&mut self) -> f64 {
        (self.next() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// An integer from 0 up to `span`, `span` left out, every value
    /// as likely as every other. A span of 0 stands for all 2^64
    /// values.
    pub(crate) fn below(&mut self, span: u64) -> u64 {
        if span == 0 {
            return self.next();
        }
        // the outputs from `limit` up would favour the low values
        let limit = u64::MAX - (u64::MAX - span + 1) % span;
        loop {
            let out = self.next();
            if out <= limit {
                return out % span;
            }
        }
    }

    /// An integer of the inclusive range, `from` no greater than
    /// `to`.
    pub(crate) fn between(&mut self, from: i64, to: i64) -> i64 {
        let span = (to as u64).wrapping_sub(from as u64).wrapping_add(1);
        (from as u64).wrapping_add(self.below(span)) as i64
    }

    /// The items in a random order: Fisher-Yates from the end.
    pub(crate) fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below(i as u64 + 1) as usize;
            items.swap(i, j);
        }
    }

    /// `n` positions of `len` without replacement, in ascending
    /// order: the positions shuffled, the first `n` taken. `n` is
    /// no greater than `len`.
    pub(crate) fn sample(&mut self, len: usize, n: usize) -> Vec<usize> {
        let mut positions: Vec<usize> = (0..len).collect();
        self.shuffle(&mut positions);
        positions.truncate(n);
        positions.sort_unstable();
        positions
    }

    /// `n` positions of `len` with replacement, in the order drawn.
    pub(crate) fn sample_replacing(&mut self, len: usize, n: usize) -> Vec<usize> {
        (0..n).map(|_| self.below(len as u64) as usize).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitmix_answers_its_published_vector() {
        // the reference implementation's outputs from the seed 0
        let mut state = 0;
        assert_eq!(splitmix(&mut state), 0xE220_A839_7B1D_CDAF);
        assert_eq!(splitmix(&mut state), 0x6E78_9E6A_A1B9_65F4);
        assert_eq!(splitmix(&mut state), 0x06C4_5D18_8009_454F);
        assert_eq!(splitmix(&mut state), 0xF88B_B8A8_724C_81EC);
    }

    #[test]
    fn xoshiro_answers_its_published_vector() {
        // the reference implementation's outputs from the state
        // 1, 2, 3, 4
        let mut stream = Stream { s: [1, 2, 3, 4] };
        let outs: Vec<u64> = (0..6).map(|_| stream.next()).collect();
        assert_eq!(
            outs,
            [
                11520,
                0,
                1509978240,
                1215971899390074240,
                1216172134540287360,
                607988272756665600
            ]
        );
    }

    /// The answers a seed gives are frozen: these were computed once
    /// by an implementation written apart from this one, and a
    /// change here is a change of the language.
    #[test]
    fn a_seed_answers_the_same_for_ever() {
        let mut stream = Stream::seeded(42);
        assert_eq!(stream.next(), 1546998764402558742);
        assert_eq!(stream.next(), 6990951692964543102);
        assert_eq!(stream.next(), 12544586762248559009);
        let mut stream = Stream::seeded(42);
        assert_eq!(stream.float(), 0.08386297105988216);
        assert_eq!(stream.float(), 0.3789802506626686);
        assert_eq!(stream.float(), 0.6800434110281394);
    }

    #[test]
    fn a_float_is_below_one_and_an_integer_inside_its_range() {
        let mut stream = Stream::seeded(7);
        for _ in 0..10_000 {
            let f = stream.float();
            assert!((0.0..1.0).contains(&f), "{f}");
            let d = stream.between(1, 6);
            assert!((1..=6).contains(&d), "{d}");
        }
        // the whole range of the integers, and a range of one
        let mut stream = Stream::seeded(7);
        let _ = stream.between(i64::MIN, i64::MAX);
        assert_eq!(stream.between(5, 5), 5);
        assert_eq!(stream.between(-3, -3), -3);
    }

    #[test]
    fn a_die_lands_on_every_face_about_as_often() {
        let mut stream = Stream::seeded(2026);
        let mut faces = [0u32; 6];
        for _ in 0..60_000 {
            faces[(stream.between(1, 6) - 1) as usize] += 1;
        }
        for (face, n) in faces.iter().enumerate() {
            assert!((9_500..10_500).contains(n), "face {}: {n}", face + 1);
        }
    }

    #[test]
    fn a_shuffle_keeps_every_item_and_a_sample_its_order() {
        let mut items: Vec<u32> = (0..100).collect();
        Stream::seeded(1).shuffle(&mut items);
        assert_ne!(items, (0..100).collect::<Vec<u32>>());
        let mut sorted = items.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..100).collect::<Vec<u32>>());
        // the same seed, the same order
        let mut again: Vec<u32> = (0..100).collect();
        Stream::seeded(1).shuffle(&mut again);
        assert_eq!(items, again);
        // a sample: distinct positions in ascending order
        let picked = Stream::seeded(1).sample(100, 10);
        assert_eq!(picked.len(), 10);
        assert!(picked.windows(2).all(|w| w[0] < w[1]), "{picked:?}");
        assert_eq!(Stream::seeded(1).sample(5, 5), [0, 1, 2, 3, 4]);
        assert!(Stream::seeded(1).sample(5, 0).is_empty());
        // with replacement: as many as asked, each a position
        let drawn = Stream::seeded(1).sample_replacing(3, 10);
        assert_eq!(drawn.len(), 10);
        assert!(drawn.iter().all(|&p| p < 3), "{drawn:?}");
    }

    #[test]
    fn a_derived_seed_tells_places_and_positions_apart() {
        let run = 42;
        assert_ne!(derive(run, 1), derive(run, 2));
        assert_ne!(derive(derive(run, 1), 1), derive(derive(run, 1), 2));
        assert_ne!(derive(1, 1), derive(2, 1));
        // and is the same every time
        assert_eq!(derive(run, 1), derive(run, 1));
    }
}
