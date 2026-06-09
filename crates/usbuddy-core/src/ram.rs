use serde::Serialize;
use sysinfo::System;

/// Estimated KV-cache memory cost per context token, in bytes.
/// Derived from llama.cpp defaults: 128 KiB per token at full precision.
pub const KV_BYTES_PER_TOKEN_DEFAULT: u64 = 131_072;

/// Minimum host RAM headroom required to avoid the red band, in bytes (1 GiB).
/// Below this threshold the OS risks swapping model weights to disk, which
/// is the primary footprint-leak vector on all platforms.
const RED_BAND_HEADROOM_BYTES: i64 = 1_073_741_824;

/// Minimum host RAM headroom for a green band result, in bytes (3 GiB).
const YELLOW_BAND_HEADROOM_BYTES: i64 = 3_221_225_472;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct MemorySnapshot {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct RamEstimateInput {
    pub model_bytes: u64,
    pub context_tokens: u32,
    pub kv_bytes_per_token: u64,
    pub runtime_overhead_bytes: u64,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FitBand {
    Green,
    Yellow,
    Red,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct RamDecision {
    pub band: FitBand,
    pub required_bytes: u64,
    pub remaining_bytes: i64,
    pub host_headroom_bytes: i64,
    pub margin_ratio: f64,
}

pub fn detect_memory() -> MemorySnapshot {
    let mut system = System::new();
    system.refresh_memory();
    MemorySnapshot {
        total_bytes: system.total_memory(),
        available_bytes: system.available_memory(),
    }
}

pub fn assess_fit(snapshot: MemorySnapshot, input: RamEstimateInput) -> RamDecision {
    let required_bytes = input
        .model_bytes
        .saturating_add(u64::from(input.context_tokens) * input.kv_bytes_per_token)
        .saturating_add(input.runtime_overhead_bytes);
    let remaining_bytes = snapshot.available_bytes as i64 - required_bytes as i64;
    let margin_ratio = if required_bytes == 0 {
        1.0
    } else {
        remaining_bytes.max(0) as f64 / required_bytes as f64
    };
    let host_headroom_bytes = remaining_bytes;
    let band = if remaining_bytes < 0 || host_headroom_bytes < RED_BAND_HEADROOM_BYTES {
        FitBand::Red
    } else if margin_ratio < 0.2 || host_headroom_bytes < YELLOW_BAND_HEADROOM_BYTES {
        FitBand::Yellow
    } else {
        FitBand::Green
    };

    RamDecision {
        band,
        required_bytes,
        remaining_bytes,
        host_headroom_bytes,
        margin_ratio,
    }
}

#[cfg(test)]
mod tests {
    use super::{FitBand, MemorySnapshot, RamEstimateInput, assess_fit};

    #[test]
    fn assigns_green_yellow_red_bands() {
        let input = RamEstimateInput {
            model_bytes: 4_000_000_000,
            context_tokens: 4_096,
            kv_bytes_per_token: 131_072,
            runtime_overhead_bytes: 500_000_000,
        };
        let green = assess_fit(
            MemorySnapshot {
                total_bytes: 16_000_000_000,
                available_bytes: 10_000_000_000,
            },
            input,
        );
        assert_eq!(green.band, FitBand::Green);

        let yellow = assess_fit(
            MemorySnapshot {
                total_bytes: 8_000_000_000,
                available_bytes: 6_200_000_000,
            },
            input,
        );
        assert_eq!(yellow.band, FitBand::Yellow);

        let red = assess_fit(
            MemorySnapshot {
                total_bytes: 8_000_000_000,
                available_bytes: 4_000_000_000,
            },
            input,
        );
        assert_eq!(red.band, FitBand::Red);
    }
}
