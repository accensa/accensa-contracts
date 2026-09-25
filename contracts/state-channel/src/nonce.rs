//! Sliding-window bitmap nonce tracking for state channels (issue #374).
//!
//! The original channel contract required strictly-increasing nonces
//! (`state.nonce > channel.nonce`), which serializes concurrent payment
//! streams: a state carrying nonce 3 arriving before nonce 2 is rejected as
//! "stale" even though both are valid, fresh commitments from the sender.
//!
//! [`NonceWindow`] tracks consumption over a 256-nonce sliding window with a
//! single 256-bit bitmap (LSB-first within each byte — the same bit order as
//! [`accensa_common::nonce::NonceBitmap`], so revoked-nonce tooling reads
//! both the same way). Any nonce inside the window may be consumed exactly
//! once, in any order; once a nonce beyond the current window arrives, the
//! window slides forward to cover it, discarding consumed-state for nonces
//! more than 256 behind. Nonces before the window base are permanently out
//! of range: they can never be re-consumed, so replays across closed or
//! settled channel epochs stay rejected.
//!
//! The window lives inside the channel record itself (see `Channel` in
//! [`crate`]), so it is written atomically with every accepted state and
//! inherits the channel entry's TTL — no separate storage key, no chance of
//! the window and the channel drifting apart.

use accensa_common::Error;
use soroban_sdk::{contracttype, BytesN, Env};

/// Number of nonces covered by one bitmap window (256 bits = 32 bytes).
pub const WINDOW_SIZE: u64 = 256;

/// A 256-nonce sliding consumption window.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NonceWindow {
    /// First nonce covered by the bitmap; always a multiple of
    /// [`WINDOW_SIZE`].
    pub base: u64,
    /// Consumption bitmap for nonces `base..base + WINDOW_SIZE`.
    /// Bit `i` (byte `i / 8`, bit `i % 8` — LSB-first) set ⇒ nonce
    /// `base + i` was already consumed.
    pub bitmap: BytesN<32>,
}

impl NonceWindow {
    /// A window covering nonces `0..256`, nothing consumed yet.
    pub fn empty(env: &Env) -> Self {
        Self {
            base: 0,
            bitmap: BytesN::from_array(env, &[0u8; 32]),
        }
    }

    /// Whether `nonce` has already been consumed.
    ///
    /// Nonces before the window base count as consumed (the window slid past
    /// them, so they are permanently unusable); nonces at or beyond
    /// `base + WINDOW_SIZE` are not yet covered and count as unconsumed.
    pub fn is_consumed(&self, nonce: u64) -> bool {
        if nonce < self.base {
            return true;
        }
        let offset = nonce - self.base;
        if offset >= WINDOW_SIZE {
            return false;
        }
        let bytes = self.bitmap.to_array();
        let byte = bytes[(offset / 8) as usize];
        (byte & (1u8 << (offset % 8))) != 0
    }

    /// Mark `nonce` as consumed.
    ///
    /// Fails with [`Error::StaleState`] if the nonce lies before the window
    /// (already lost to a slide) or its bit is already set (replay).
    /// Consuming a nonce at or beyond `base + WINDOW_SIZE` slides the window
    /// forward so the nonce lands inside it; bits for nonces more than 256
    /// behind the new base are discarded.
    pub fn consume(&mut self, env: &Env, nonce: u64) -> Result<(), Error> {
        if nonce < self.base {
            return Err(Error::StaleState);
        }

        let mut offset = nonce - self.base;
        if offset >= WINDOW_SIZE {
            // Slide the window to the 256-aligned bucket containing `nonce`.
            self.base = nonce - (nonce % WINDOW_SIZE);
            self.bitmap = BytesN::from_array(env, &[0u8; 32]);
            offset = nonce - self.base;
        }

        let mut bytes = self.bitmap.to_array();
        let byte_index = (offset / 8) as usize;
        let bit = 1u8 << (offset % 8);
        if bytes[byte_index] & bit != 0 {
            return Err(Error::StaleState);
        }
        bytes[byte_index] |= bit;
        self.bitmap = BytesN::from_array(env, &bytes);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fresh_window_consumes_in_any_order() {
        let env = Env::default();
        let mut w = NonceWindow::empty(&env);
        assert!(!w.is_consumed(7));

        w.consume(&env, 5).unwrap();
        w.consume(&env, 3).unwrap();
        w.consume(&env, 4).unwrap();

        assert!(w.is_consumed(3));
        assert!(w.is_consumed(4));
        assert!(w.is_consumed(5));
        assert!(!w.is_consumed(6));
        assert!(!w.is_consumed(2));
        assert_eq!(w.base, 0);
    }

    #[test]
    fn test_replay_of_consumed_nonce_fails() {
        let env = Env::default();
        let mut w = NonceWindow::empty(&env);
        w.consume(&env, 9).unwrap();
        assert_eq!(w.consume(&env, 9), Err(Error::StaleState));
        assert!(w.is_consumed(9));
    }

    #[test]
    fn test_out_of_order_arrival_does_not_block_siblings() {
        let env = Env::default();
        let mut w = NonceWindow::empty(&env);
        // 8 arrives first; 1..=7 are still free.
        w.consume(&env, 8).unwrap();
        for n in 1..8u64 {
            w.consume(&env, n).unwrap();
        }
        assert_eq!(w.consume(&env, 1), Err(Error::StaleState));
    }

    #[test]
    fn test_window_bit_exhaustion_slides_forward() {
        let env = Env::default();
        let mut w = NonceWindow::empty(&env);
        for n in 0..WINDOW_SIZE {
            w.consume(&env, n).unwrap();
        }
        // Full window: every covered nonce is consumed.
        for n in 0..WINDOW_SIZE {
            assert!(w.is_consumed(n));
        }
        assert_eq!(w.consume(&env, 0), Err(Error::StaleState));

        // First nonce beyond the window slides it to base 256.
        w.consume(&env, WINDOW_SIZE).unwrap();
        assert_eq!(w.base, WINDOW_SIZE);
        assert!(w.is_consumed(WINDOW_SIZE));
        assert!(!w.is_consumed(WINDOW_SIZE + 1));
        // Anything more than 256 behind the new base is now out of range.
        assert!(w.is_consumed(0));
        assert_eq!(w.consume(&env, 1), Err(Error::StaleState));
        // Still-covered unconsumed nonces remain usable.
        w.consume(&env, WINDOW_SIZE + 1).unwrap();
        assert!(w.is_consumed(WINDOW_SIZE + 1));
    }

    #[test]
    fn test_slide_preserves_in_window_bits() {
        let env = Env::default();
        let mut w = NonceWindow::empty(&env);
        w.consume(&env, 300).unwrap();
        assert_eq!(w.base, 256);
        // 300 landed in the new window; 301 is fresh, 300 is not.
        assert!(w.is_consumed(300));
        assert!(!w.is_consumed(301));
        w.consume(&env, 301).unwrap();
        assert!(w.is_consumed(301));
        // 400 < 512, still inside the window at base 256.
        w.consume(&env, 400).unwrap();
        assert_eq!(w.base, 256);
        // 512 triggers the next slide.
        w.consume(&env, 512).unwrap();
        assert_eq!(w.base, 512);
        assert_eq!(w.consume(&env, 300), Err(Error::StaleState));
    }
}
