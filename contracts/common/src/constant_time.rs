//! Constant-time cryptographic comparison (issue #436).
//!
//! Comparing secret-derived byte strings with `==` or `slice::eq` is a timing
//! oracle: the standard implementations short-circuit at the first differing
//! byte, so the time a comparison takes leaks the length of the shared prefix.
//! An attacker who can measure call latency can recover a token, MAC, or
//! signature one byte at a time.
//!
//! [`constant_time_eq`] compares two byte slices in time that depends only on
//! their *lengths*: it walks every byte of both inputs, accumulates each
//! difference with a bitwise OR, and inspects the accumulator only once, at
//! the very end. There is no early return and no branch on a byte value, and
//! [`core::hint::black_box`] stops the optimizer from rediscovering the
//! short-circuit and undoing the mitigation.
//!
//! # Scope
//!
//! This is a best-effort software mitigation for the guest, not a hardware
//! guarantee. Soroban's gas metering means a caller can already observe
//! operation counts, and cache/timing effects below the guest are outside a
//! contract's control. Use it for the comparisons where a byte-level prefix
//! leak is the threat — MACs, bearer tokens, commitment/digest equality — and
//! keep the surrounding interface from branching on the result where possible.
//!
//! # Active Call Sites
//!
//! - `contracts/state-channel/src/htlc.rs`: Verifies HTLC preimages against
//!   hash locks in constant time to prevent timing leakage on secret-derived
//!   preimage bytes.
//!
//! Note: other audited comparison sites compare purely public data and
//! intentionally use standard equality: the guardian-address check in
//! `contracts/cross-chain/src/wormhole.rs` and the Merkle-root check in
//! `contracts/receipt-shard/src/lib.rs`.

use core::hint::black_box;

/// Compare `a` and `b` without short-circuiting on the first difference.
///
/// Returns `true` iff the slices have the same length and identical contents.
///
/// Every byte of both inputs is read, folded into a single `u32` accumulator
/// with `|=`, and the accumulator is tested exactly once at the end. A length
/// difference is folded in as a non-zero seed before the loop, so it too is
/// detected without a conditional early exit.
///
/// # Examples
///
/// ```
/// use accensa_common::constant_time::constant_time_eq;
///
/// assert!(constant_time_eq(b"same", b"same"));
/// assert!(!constant_time_eq(b"same", b"sAmE"));
/// assert!(!constant_time_eq(b"same", b"same!"));
/// ```
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    // Seed with the length difference. A mismatch makes this non-zero, and no
    // subsequent step can clear a set bit, so the length check is folded into
    // the same accumulation rather than being a separate early return.
    let mut diff: u32 = (a.len() ^ b.len()) as u32;

    let common = if a.len() < b.len() { a.len() } else { b.len() };

    // Compare the common prefix, byte by byte.
    let mut i = 0;
    while i < common {
        diff |= (a[i] ^ b[i]) as u32;
        i += 1;
    }

    // Fold in whichever input has a tail. These branches are on the (public)
    // lengths only, never on the byte values, and a length difference has
    // already been folded into `diff` above.
    let mut j = common;
    while j < a.len() {
        diff |= a[j] as u32;
        j += 1;
    }
    let mut k = common;
    while k < b.len() {
        diff |= b[k] as u32;
        k += 1;
    }

    // The single, final inspection of the accumulated difference. `black_box`
    // prevents the compiler from proving the loop result and collapsing it
    // back into a short-circuiting comparison.
    black_box(diff) == 0
}

#[cfg(test)]
mod tests {
    use super::constant_time_eq;

    #[test]
    fn identical_slices_are_equal() {
        assert!(constant_time_eq(b"", b""));
        assert!(constant_time_eq(b"\x00", b"\x00"));
        assert!(constant_time_eq(&[0xff; 64], &[0xff; 64]));
        assert!(constant_time_eq(b"accensa", b"accensa"));
    }

    #[test]
    fn empty_inputs_are_equal() {
        assert!(constant_time_eq(b"", b""));
        assert!(!constant_time_eq(b"", b"a"));
        assert!(!constant_time_eq(b"a", b""));
    }

    #[test]
    fn first_byte_difference_is_detected() {
        let a = [0x11, 0x22, 0x33, 0x44];
        let b = [0x99, 0x22, 0x33, 0x44];
        assert!(!constant_time_eq(&a, &b));
        assert!(!constant_time_eq(&b, &a));
    }

    #[test]
    fn last_byte_difference_is_detected() {
        let a = [0x11, 0x22, 0x33, 0x44];
        let b = [0x11, 0x22, 0x33, 0x99];
        assert!(!constant_time_eq(&a, &b));
        assert!(!constant_time_eq(&b, &a));
    }

    #[test]
    fn length_mismatch_both_directions_detected() {
        assert!(!constant_time_eq(b"", b"\x00"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"abcd", b"abc"));
        assert!(!constant_time_eq(b"key", b"key\x00"));
        assert!(!constant_time_eq(b"key\x00", b"key"));
    }

    #[test]
    fn a_difference_at_every_position_is_detected() {
        // Flip each byte in turn: no position may be missed.
        let base = [0x5au8; 32];
        let mut probe = base;
        for i in 0..probe.len() {
            probe[i] = 0xa5;
            assert!(
                !constant_time_eq(&base, &probe),
                "mismatch at byte {i} was not detected"
            );
            assert!(!constant_time_eq(&probe, &base));
            probe[i] = base[i];
        }
        assert!(constant_time_eq(&base, &probe));
    }

    #[test]
    fn zero_padding_does_not_hide_a_length_difference() {
        // Both contain only zeros, differing solely in length.
        let short = [0u8; 16];
        let long = [0u8; 32];
        assert!(!constant_time_eq(&short, &long));
        assert!(constant_time_eq(&short, &short));
    }
}
