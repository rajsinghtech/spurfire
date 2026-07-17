//! Deterministic playable-territory sizing.

/// Minimum active-territory radius in metres.
pub const MIN_PLAYABLE_RADIUS_M: u32 = 450;
/// Maximum active-territory radius in metres.
pub const MAX_PLAYABLE_RADIUS_M: u32 = 1_500;

/// Computes `clamp(450, 250 * sqrt(player_count), 1500)` without floats.
///
/// Fractional metres are floored. Computing `sqrt(62_500 * n)` directly is
/// equivalent to `floor(250 * sqrt(n))` and keeps every platform bit-identical.
#[must_use]
pub const fn playable_radius_m(player_count: u32) -> u32 {
    let radicand = 62_500_u64 * player_count as u64;
    let unbounded = integer_sqrt(radicand) as u32;
    if unbounded < MIN_PLAYABLE_RADIUS_M {
        MIN_PLAYABLE_RADIUS_M
    } else if unbounded > MAX_PLAYABLE_RADIUS_M {
        MAX_PLAYABLE_RADIUS_M
    } else {
        unbounded
    }
}

const fn integer_sqrt(value: u64) -> u64 {
    if value < 2 {
        return value;
    }

    let mut low = 1_u64;
    let mut high = value / 2 + 1;
    let mut answer = 1_u64;
    while low <= high {
        let middle = low + (high - low) / 2;
        if middle <= value / middle {
            answer = middle;
            low = middle + 1;
        } else {
            high = middle - 1;
        }
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radius_examples_are_exact_and_clamped() {
        let examples = [
            (0, 450),
            (1, 450),
            (2, 450),
            (3, 450),
            (4, 500),
            (6, 612),
            (8, 707),
            (16, 1_000),
            (35, 1_479),
            (36, 1_500),
            (100, 1_500),
        ];
        for (players, expected) in examples {
            assert_eq!(playable_radius_m(players), expected, "players={players}");
        }
    }

    #[test]
    fn radius_is_monotonic() {
        let mut previous = 0;
        for players in 0..=10_000 {
            let radius = playable_radius_m(players);
            assert!(radius >= previous);
            previous = radius;
        }
    }
}
