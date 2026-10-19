//! Quorum-decay math for [`Governance`] (issue #392).
//!
//! A proposal's effective quorum starts at the initial `threshold_bps` set at
//! construction and decays **linearly** toward a protocol-safety floor as the
//! voting window elapses, so a proposal that has sat inactive for most of its
//! window needs less "yes" weight to pass. The floor is absolute: decay never
//! lowers the required quorum beneath [`QUORUM_FLOOR_BPS`].

/// Lower bound on the effective quorum, in basis points of total weight.
///
/// The safety minimum: even a fully-window-decayed proposal must still clear
/// this much of the total weight, so a single heavyweight member can never
/// push through a proposal that the rest of the body opposes.
pub const QUORUM_FLOOR_BPS: u32 = 3_500;

/// Effective quorum in basis points for a proposal that has `elapsed` ledgers
/// of its `window`-ledger voting window already behind it.
///
/// The decay is linear: `max(start_bps, QUORUM_FLOOR_BPS)` at the first
/// ledger, `QUORUM_FLOOR_BPS` once `elapsed >= window`, and a linear ramp in
/// between. Computed with integer arithmetic only (`u64` intermediate, no
/// fractional loss). The floor is absolute: a `start_bps` below it is raised
/// to the floor, so the result is always in
/// `[QUORUM_FLOOR_BPS, max(start_bps, QUORUM_FLOOR_BPS)]`.
pub fn current_quorum_bps(start_bps: u32, elapsed: u32, window: u32) -> u32 {
    if window == 0 {
        return QUORUM_FLOOR_BPS;
    }
    if elapsed >= window {
        return QUORUM_FLOOR_BPS;
    }

    let start = u64::from(start_bps.max(QUORUM_FLOOR_BPS));
    let floor = u64::from(QUORUM_FLOOR_BPS);
    let span = start - floor;

    let decayed = span * u64::from(elapsed) / u64::from(window);
    let current = start - decayed;

    u32::try_from(current).unwrap_or(QUORUM_FLOOR_BPS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_window_returns_the_floor() {
        assert_eq!(current_quorum_bps(5_000, 0, 0), QUORUM_FLOOR_BPS);
        assert_eq!(current_quorum_bps(2_000, 0, 0), QUORUM_FLOOR_BPS);
    }

    #[test]
    fn elapsed_at_or_past_the_window_returns_the_floor() {
        assert_eq!(current_quorum_bps(5_000, 100, 100), QUORUM_FLOOR_BPS);
        assert_eq!(current_quorum_bps(5_000, 101, 100), QUORUM_FLOOR_BPS);
    }

    #[test]
    fn elapsed_zero_returns_the_start() {
        assert_eq!(current_quorum_bps(5_000, 0, 100), 5_000);
        assert_eq!(current_quorum_bps(4_000, 0, 1), 4_000);
    }

    #[test]
    fn start_below_the_floor_is_clamped_up() {
        // The bug: a start below the floor previously stayed below it.
        assert_eq!(current_quorum_bps(2_000, 0, 100), QUORUM_FLOOR_BPS);
        assert_eq!(current_quorum_bps(2_000, 50, 100), QUORUM_FLOOR_BPS);
        assert_eq!(current_quorum_bps(2_000, 99, 100), QUORUM_FLOOR_BPS);
    }

    #[test]
    fn midpoint_is_exact() {
        // span = 5000 - 3500 = 1500; 1500 * 50 / 100 = 750; 5000 - 750 = 4250.
        assert_eq!(current_quorum_bps(5_000, 50, 100), 4_250);
    }

    #[test]
    fn odd_window_truncates_the_decay() {
        // span = 4000 - 3500 = 500; 500 * 1 / 3 = 166 (integer division);
        // 4000 - 166 = 3834.
        assert_eq!(current_quorum_bps(4_000, 1, 3), 3_834);
    }

    #[test]
    fn never_leaves_the_documented_interval() {
        for elapsed in 0..=100u32 {
            for start in [0u32, 1, 2_000, 3_499, 3_500, 3_501, 5_000, 10_000] {
                let q = current_quorum_bps(start, elapsed, 100);
                assert!(
                    q >= QUORUM_FLOOR_BPS,
                    "start={start} elapsed={elapsed} q={q}"
                );
                assert!(
                    q <= start.max(QUORUM_FLOOR_BPS),
                    "start={start} elapsed={elapsed} q={q}"
                );
            }
        }
    }
}
