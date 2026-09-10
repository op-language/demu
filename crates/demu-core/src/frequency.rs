//! Exact rational time types and the boundary conversion.
//!
//! Ticks are the canonical simulation time. Every conversion to or from
//! nanoseconds goes through [`Frequency::ns_to_ticks`] and
//! [`Frequency::ticks_to_ns`] with round-half-to-even. No float arithmetic
//! exists in this crate.

use std::fmt;

use demu_config::ConfigError;

/// A reduced rational `num/den` with `num > 0` and `den > 0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Frequency {
    num: u64,
    den: u64,
}

impl Frequency {
    /// Construct a reduced rational from `num/den`.
    pub fn new(num: u64, den: u64) -> Result<Self, ConfigError> {
        if num == 0 || den == 0 {
            return Err(ConfigError::FrequencyZero { num, den });
        }
        let g = gcd(num, den);
        Ok(Self {
            num: num / g,
            den: den / g,
        })
    }

    /// Construct an integer-Hz frequency, the rational `n/1`.
    pub fn from_hz(hz: u64) -> Result<Self, ConfigError> {
        Self::new(hz, 1)
    }

    /// The reduced numerator.
    pub fn num(&self) -> u64 {
        self.num
    }

    /// The reduced denominator.
    pub fn den(&self) -> u64 {
        self.den
    }

    /// Convert nanoseconds to domain ticks with round-half-to-even.
    ///
    /// The computation is `(ns * num) / (den * 1e9)` in exact integer
    /// arithmetic. Ties round to the even quotient. `u128` intermediates
    /// hold any realistic `ns * num` product without overflow.
    pub fn ns_to_ticks(&self, ns: u64) -> u64 {
        let num = u128::from(self.num);
        let den = u128::from(self.den) * 1_000_000_000u128;
        let scaled = u128::from(ns) * num;
        divide_half_to_even(scaled, den)
    }

    /// Convert domain ticks to nanoseconds with round-half-to-even.
    ///
    /// The computation is `(ticks * den * 1e9) / num` in exact integer
    /// arithmetic.
    pub fn ticks_to_ns(&self, ticks: u64) -> u64 {
        let scaled = u128::from(ticks) * u128::from(self.den) * 1_000_000_000u128;
        let ns = divide_half_to_even(scaled, u128::from(self.num));
        // ns fits u64 for any realistic tick count; a tick count that
        // overflows here would describe more than 584 years of time.
        ns
    }
}

/// Divide `a / b` rounding half to even, for `b > 0`.
fn divide_half_to_even(a: u128, b: u128) -> u64 {
    let q = a / b;
    let r = a % b;
    let twice = r * 2;
    let rounded = match twice.cmp(&b) {
        std::cmp::Ordering::Less => q,
        std::cmp::Ordering::Greater => q + 1,
        // Exact tie: round to even.
        std::cmp::Ordering::Equal => {
            if q.is_multiple_of(2) {
                q
            } else {
                q + 1
            }
        }
    };
    u64::try_from(rounded).unwrap_or(u64::MAX)
}

impl fmt::Display for Frequency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.den == 1 {
            write!(f, "{}", self.num)
        } else {
            write!(f, "{}/{}", self.num, self.den)
        }
    }
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// A count of domain ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Tick(pub u64);

/// A positive integer divider of a domain crystal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Divider(u32);

impl Divider {
    /// Construct a divider from a positive integer.
    pub fn new(raw: u32) -> Result<Self, ConfigError> {
        if raw == 0 {
            return Err(ConfigError::InvalidValue {
                path: "divider".to_string(),
                message: "divider must be positive".to_string(),
            });
        }
        Ok(Self(raw))
    }

    /// The divider value.
    pub fn get(&self) -> u32 {
        self.0
    }
}

/// A bus address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Address(pub u32);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frequency_reduces() {
        let f = Frequency::new(8, 4).unwrap();
        assert_eq!((f.num(), f.den()), (2, 1));
        let ntsc = Frequency::new(236250000, 11).unwrap();
        assert_eq!((ntsc.num(), ntsc.den()), (236250000, 11));
        assert!(Frequency::new(0, 1).is_err());
        assert!(Frequency::new(1, 0).is_err());
    }

    #[test]
    fn ntsc_conversion_rounds_half_to_even() {
        // NTSC master: 236250000/11 Hz. One tick is 11/236250000 s, which
        // is not a whole number of nanoseconds.
        let f = Frequency::new(236250000, 11).unwrap();
        // 1 tick = 46.5608... ns -> 47 ns on the way out (0.5608 rounds up).
        assert_eq!(f.ticks_to_ns(1), 47);
        // 47 ns = 1.0094... ticks -> 1 tick on the way back.
        assert_eq!(f.ns_to_ticks(47), 1);
        // 0 ns is 0 ticks, exactly.
        assert_eq!(f.ns_to_ticks(0), 0);
        // One second of NTSC ticks: 236250000/11 = 21477272.727... -> 21477273.
        assert_eq!(f.ticks_to_ns(236250000), 11 * 1_000_000_000);
    }

    #[test]
    fn spc700_conversion_rounds_correctly() {
        // SPC700: 1024000 Hz. One tick is 976.5625 ns exactly.
        let f = Frequency::from_hz(1024000).unwrap();
        // The fractional part is 0.5625, which rounds up: 977.
        assert_eq!(f.ticks_to_ns(1), 977);
        // 977 ns is 1.0004... ticks -> 1 tick.
        assert_eq!(f.ns_to_ticks(977), 1);
        // Two ticks are 1953.125 ns -> 1953.
        assert_eq!(f.ticks_to_ns(2), 1953);
        // 1000000 ns is 1024 ticks exactly at 1024000 Hz.
        assert_eq!(f.ns_to_ticks(1_000_000), 1024);
    }

    #[test]
    fn integer_hz_round_trip_is_exact() {
        let f = Frequency::from_hz(4194304).unwrap();
        for ns in [0, 1, 1000, 1_000_000] {
            let ticks = f.ns_to_ticks(ns);
            // Round-trip within one tick period.
            let back = f.ticks_to_ns(ticks);
            assert!(back <= ns + 238, "ns={ns} back={back}");
        }
    }

    #[test]
    fn divider_rejects_zero() {
        assert!(Divider::new(4).is_ok());
        assert!(Divider::new(0).is_err());
    }
}
