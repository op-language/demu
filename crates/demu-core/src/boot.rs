//! Boot-state initialization.
//!
//! A device boot state comes from the configuration: ROM file, RAM
//! pattern, hex dump, or CPU register presets. The RAM random pattern
//! uses a deterministic seeded SplitMix64 generator, so the same
//! configuration boots identically everywhere.

use std::path::Path;

use demu_config::model::RamPattern;

/// Fill a RAM block from a pattern.
///
/// The pattern is repeated to fill the whole buffer when it is shorter.
pub fn fill_ram(buffer: &mut [u8], pattern: RamPattern) {
    match pattern {
        RamPattern::Zero => buffer.fill(0),
        RamPattern::Random { seed } => {
            let mut rng = SplitMix64::new(seed);
            let (chunks, rest_bytes) = buffer.as_chunks_mut::<8>();
            for chunk in chunks {
                let value = rng.next_value();
                chunk.copy_from_slice(&value.to_le_bytes());
            }
            if !rest_bytes.is_empty() {
                let value = rng.next_value().to_le_bytes();
                rest_bytes.copy_from_slice(&value[..rest_bytes.len()]);
            }
        }
    }
}

/// Load a ROM file into a device buffer.
///
/// # Errors
///
/// Returns an error when the file is missing or larger than the buffer.
pub fn load_rom(buffer: &mut [u8], path: &Path) -> Result<(), std::io::Error> {
    let data = std::fs::read(path)?;
    if data.len() > buffer.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "ROM file {} is {} bytes but the device holds {}",
                path.display(),
                data.len(),
                buffer.len()
            ),
        ));
    }
    buffer[..data.len()].copy_from_slice(&data);
    Ok(())
}

/// A deterministic SplitMix64 generator.
#[derive(Debug, Clone)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    /// A generator seeded from `seed`.
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// The next 64-bit value.
    pub fn next_value(&mut self) -> u64 {
        splitmix64(&mut self.state)
    }
}

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_pattern_is_deterministic() {
        let mut a = vec![0u8; 64];
        let mut b = vec![0u8; 64];
        fill_ram(&mut a, RamPattern::Random { seed: 42 });
        fill_ram(&mut b, RamPattern::Random { seed: 42 });
        assert_eq!(a, b, "same seed produces the same bytes");
        assert!(a.iter().any(|b| *b != 0), "the pattern is not all zero");

        let mut c = vec![0u8; 64];
        fill_ram(&mut c, RamPattern::Random { seed: 7 });
        assert_ne!(a, c, "different seeds differ");
    }

    #[test]
    fn zero_pattern_clears() {
        let mut a = vec![0xFFu8; 32];
        fill_ram(&mut a, RamPattern::Zero);
        assert!(a.iter().all(|b| *b == 0));
    }

    #[test]
    fn splitmix_matches_reference_vector() {
        // The SplitMix64 reference test vector: seed 0 first value.
        let mut g = SplitMix64::new(0);
        assert_eq!(g.next_value(), 0xE220_A839_7B1D_CDAF);
    }
}
