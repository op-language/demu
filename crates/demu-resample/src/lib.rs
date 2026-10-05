//! Integer-ratio audio resampler.
//!
//! The resampler converts a mono `i16` stream at any positive integer
//! source rate to a fixed 48000 Hz output stream using exact rational
//! sample-and-hold mapping. There is no interpolation and no floating
//! point arithmetic anywhere in the crate: every position comparison and
//! division happens in exact `u128` integer math.
//!
//! # Mapping
//!
//! Output sample `i` (0-based) sits at the exact rational source position
//! `P_i = i * source_rate / 48_000` and takes the value of the source
//! sample at index `s_i = floor(P_i)`. Over a whole session the output is
//! exactly `sample[floor(i * source_rate / 48_000)]` for
//! `i = 0..output_count()`.
//!
//! # Incremental accumulator
//!
//! The resampler keeps a phase accumulator `num` that always equals
//! `next_output * source_rate`, an output counter `next_output`, and a
//! source counter `pushed`. Feeding `len` samples advances `pushed` by
//! `len`, and the resampler then emits, in order, every pending output
//! `i` with `i * source_rate < pushed * 48_000`. Both sides of the
//! comparison fit `u128` without overflow for any feedable stream, so the
//! rational condition is evaluated exactly, and emission reads the source
//! index `s = num / 48_000` with integer floor division. At any instant
//! every output whose position `P_i < pushed` has been emitted, so a
//! drain once per simulated frame carries exactly that frame's resampled
//! audio.
//!
//! # Split invariance
//!
//! The accumulator is stateful, so any split of the same source stream
//! into `push` calls yields a bit-identical concatenated output, and
//! interleaved `take_output` drains do not change it. After each `push`
//! returns, the next pending source index is at or beyond the pushed
//! count, so every emission reads only the samples of the current call;
//! no cross-call buffering is needed.

/// The fixed output rate every resampler feeds, in Hz.
pub const TARGET_RATE: u32 = 48_000;

/// Wide form of [`TARGET_RATE`] for the exact `u128` position arithmetic.
const TARGET_RATE_U128: u128 = TARGET_RATE as u128;

/// All failure modes of [`Resampler`] construction.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ResampleError {
    /// The source rate is zero.
    #[error("source rate {rate} is invalid: the rate must be a positive integer")]
    SourceRateZero {
        /// The rejected rate in Hz.
        rate: u32,
    },
}

/// A sample-and-hold resampler from one fixed source rate to
/// [`TARGET_RATE`].
///
/// Construct with [`Resampler::new`], feed source samples with
/// [`Resampler::push`], and drain produced output with
/// [`Resampler::take_output`]. Output sample `i` carries the value of the
/// source sample at index `floor(i * source_rate / 48_000)`, so a whole
/// session reproduces the source stream with floor-mapped rational timing
/// and no interpolation.
#[derive(Debug, Clone)]
pub struct Resampler {
    /// The validated positive source rate in Hz.
    source_rate: u32,
    /// Phase accumulator. Equals `next_output * source_rate` exactly.
    num: u128,
    /// Output samples emitted since construction or reset.
    next_output: u128,
    /// Source samples seen since construction or reset.
    pushed: u128,
    /// Output samples produced since the last drain.
    output: Vec<i16>,
}

impl Resampler {
    /// Create a resampler for a positive integer source rate in Hz.
    ///
    /// This is the only entry point, so an instance always carries a
    /// usable rate.
    pub fn new(source_rate: u32) -> Result<Self, ResampleError> {
        if source_rate == 0 {
            return Err(ResampleError::SourceRateZero { rate: source_rate });
        }
        Ok(Self {
            source_rate,
            num: 0,
            next_output: 0,
            pushed: 0,
            output: Vec::new(),
        })
    }

    /// Feed source samples at the source rate and produce every output
    /// sample whose source position is covered by the samples pushed so
    /// far.
    ///
    /// The call advances the source position by `samples.len()` and then
    /// emits, in order, every pending output `i` with
    /// `i * source_rate < pushed * 48_000`, where `pushed` counts the
    /// samples seen including this call. Every emitted output reads an
    /// index inside this call's slice. The produced samples accumulate
    /// until [`Resampler::take_output`] drains them.
    pub fn push(&mut self, samples: &[i16]) {
        // Global index of `samples[0]` in the source stream.
        let base = self.pushed;
        let served = base + samples.len() as u128;
        // The comparison `next_output * source_rate < pushed * 48_000` is
        // exact here: `num` carries the left side and both sides stay far
        // below the u128 bounds for any feedable stream.
        let limit = served * TARGET_RATE_U128;
        while self.num < limit {
            // `num / 48_000` is the source index `floor(P_i)`. It stays
            // within `[base, served)`, so the difference below indexes
            // into this call's slice and always fits usize.
            let local = ((self.num / TARGET_RATE_U128) - base) as usize;
            self.output.push(samples[local]);
            self.num += u128::from(self.source_rate);
            self.next_output += 1;
        }
        self.pushed = served;
    }

    /// Drain the 48 kHz output produced so far.
    ///
    /// Returns the samples emitted since the last drain in emission
    /// order, leaving the resampler ready to produce more. The returned
    /// vector is empty when no output is pending.
    pub fn take_output(&mut self) -> Vec<i16> {
        std::mem::take(&mut self.output)
    }

    /// Source samples pushed since construction or reset, the source tick
    /// count.
    ///
    /// The count saturates at `u64::MAX`, a total far beyond any feedable
    /// stream.
    pub fn source_ticks(&self) -> u64 {
        u64::try_from(self.pushed).unwrap_or(u64::MAX)
    }

    /// Output samples emitted since construction or reset.
    ///
    /// The count saturates at `u64::MAX`, a total far beyond any feedable
    /// stream.
    pub fn output_count(&self) -> u64 {
        u64::try_from(self.next_output).unwrap_or(u64::MAX)
    }

    /// Reset the accumulator state to the freshly-constructed condition.
    ///
    /// The source rate is kept. The source position, the output count,
    /// and any pending output are cleared, so the next fed samples are
    /// treated as the start of a new stream.
    pub fn reset(&mut self) {
        self.num = 0;
        self.next_output = 0;
        self.pushed = 0;
        self.output.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-random source stream, independent of the
    /// resampler under test.
    fn pseudo_random(len: usize, seed: u64) -> Vec<i16> {
        let mut state = seed;
        (0..len)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                (state >> 33) as i16
            })
            .collect()
    }

    /// Expected whole-session output from independent floor arithmetic:
    /// output `i` is `input[floor(i * source_rate / 48_000)]`, and the
    /// count is the number of `i >= 0` with
    /// `i * source_rate < pushed * 48_000`.
    fn expected_output(input: &[i16], source_rate: u32) -> Vec<i16> {
        let src = u128::from(source_rate);
        let pushed = input.len() as u128;
        if pushed == 0 {
            return Vec::new();
        }
        let count = ((pushed * 48_000 - 1) / src + 1) as usize;
        (0..count)
            .map(|i| input[(i as u128 * src / 48_000) as usize])
            .collect()
    }

    #[test]
    fn new_rejects_zero_rate() {
        assert!(matches!(
            Resampler::new(0),
            Err(ResampleError::SourceRateZero { .. })
        ));
        assert!(Resampler::new(1).is_ok());
        assert!(Resampler::new(48_000).is_ok());
        assert!(Resampler::new(1_789_773).is_ok());
    }

    #[test]
    fn identity_rate_48000_returns_input_unchanged() {
        for len in [0, 1, 2, 3, 7, 100, 1001] {
            let input = pseudo_random(len, 0x1234_5678);
            let mut resampler = Resampler::new(TARGET_RATE).unwrap();
            resampler.push(&input);
            assert_eq!(resampler.take_output(), input, "len={len}");
            assert_eq!(resampler.output_count(), len as u64);
            assert_eq!(resampler.source_ticks(), len as u64);
            // A drain with nothing pending returns an empty vector.
            assert!(resampler.take_output().is_empty());
        }
    }

    #[test]
    fn rate_96000_takes_every_second_source_sample() {
        let input: Vec<i16> = (0..20).map(|v| v as i16 * 111).collect();
        let mut resampler = Resampler::new(96_000).unwrap();
        resampler.push(&input);
        let output = resampler.take_output();
        assert_eq!(output.len(), 10);
        assert_eq!(
            output,
            vec![0, 222, 444, 666, 888, 1110, 1_332, 1_554, 1_776, 1_998]
        );
    }

    #[test]
    fn rate_24000_repeats_every_source_sample() {
        let input = [10, -20, 30];
        let mut resampler = Resampler::new(24_000).unwrap();
        resampler.push(&input);
        assert_eq!(resampler.take_output(), vec![10, 10, -20, -20, 30, 30]);
    }

    #[test]
    fn rate_8000_matches_independent_floor_math() {
        let input = [7, 13, 29, 41, 53, 67, 71, 83, 97, 101];
        let mut resampler = Resampler::new(8_000).unwrap();
        resampler.push(&input);
        let output = resampler.take_output();
        assert_eq!(output, expected_output(&input, 8_000));
        // Each position step is 8000/48000 = 1/6, so floor mapping gives
        // every source sample six consecutive outputs.
        assert_eq!(output.len(), 60);
        for (i, value) in output.iter().enumerate() {
            assert_eq!(*value, input[i / 6], "i={i}");
        }
    }

    #[test]
    fn split_pushes_match_single_push() {
        let rate = 44_100;
        let input = pseudo_random(10_000, 0xdead_beef);
        let mut whole = Resampler::new(rate).unwrap();
        whole.push(&input);
        let whole_output = whole.take_output();

        // One split at an arbitrary boundary.
        let (a, b) = input.split_at(3_141);
        let mut split = Resampler::new(rate).unwrap();
        split.push(a);
        split.push(b);
        assert_eq!(split.take_output(), whole_output);

        // Many small pushes with a drain after each call.
        let mut dribble = Resampler::new(rate).unwrap();
        let mut dribbled = Vec::new();
        for chunk in input.chunks(7) {
            dribble.push(chunk);
            dribbled.extend(dribble.take_output());
        }
        dribbled.extend(dribble.take_output());
        assert_eq!(dribbled, whole_output);
        assert_eq!(dribble.output_count(), whole.output_count());
        assert_eq!(dribble.source_ticks(), whole.source_ticks());
    }

    #[test]
    fn fresh_resamplers_are_bit_identical() {
        let rate = 32_768;
        let input = pseudo_random(40_000, 7);
        let mut first = Resampler::new(rate).unwrap();
        first.push(&input);
        let mut second = Resampler::new(rate).unwrap();
        second.push(&input);
        let left = first.take_output();
        let right = second.take_output();
        assert_eq!(left, right);
        assert_eq!(first.output_count(), second.output_count());
    }

    #[test]
    fn interleaved_drains_match_single_drain() {
        let rate = 22_050;
        let input = pseudo_random(20_000, 99);
        let mut whole = Resampler::new(rate).unwrap();
        whole.push(&input);
        let whole_output = whole.take_output();

        let mut mixed = Resampler::new(rate).unwrap();
        let mut collected = Vec::new();
        for (n, chunk) in input.chunks(1_000).enumerate() {
            mixed.push(chunk);
            if n % 2 == 0 {
                collected.extend(mixed.take_output());
            }
        }
        collected.extend(mixed.take_output());
        assert_eq!(collected, whole_output);
    }

    #[test]
    fn long_run_output_count_matches_floor_expectation() {
        let rate = 1_789_773;
        let pushed: u64 = 1_000_000;
        let input = pseudo_random(pushed as usize, 0xfeed_face);

        let mut resampler = Resampler::new(rate).unwrap();
        resampler.push(&input);
        assert_eq!(resampler.source_ticks(), pushed);

        // Outputs are the count of i >= 0 with i * rate < pushed * 48_000,
        // which is floor((pushed * 48_000 - 1) / rate) + 1 in u128.
        let expected = (u128::from(pushed) * 48_000 - 1) / u128::from(rate) + 1;
        assert_eq!(u128::from(resampler.output_count()), expected);
        let output = resampler.take_output();
        assert_eq!(output.len() as u128, expected);
        assert_eq!(output, expected_output(&input, rate));
    }

    #[test]
    fn reset_restores_fresh_accumulator_state() {
        let rate = 44_100;
        let input = pseudo_random(5_000, 0xfeed_face);

        let mut fresh = Resampler::new(rate).unwrap();
        fresh.push(&input);
        let fresh_output = fresh.take_output();
        let fresh_ticks = fresh.source_ticks();
        let fresh_count = fresh.output_count();

        let mut reset = Resampler::new(rate).unwrap();
        reset.push(&input[..1_000]);
        assert!(reset.output_count() > 0);
        reset.reset();
        assert_eq!(reset.source_ticks(), 0);
        assert_eq!(reset.output_count(), 0);
        assert!(reset.take_output().is_empty());
        reset.push(&input);
        assert_eq!(reset.take_output(), fresh_output);
        assert_eq!(reset.source_ticks(), fresh_ticks);
        assert_eq!(reset.output_count(), fresh_count);
    }
}
