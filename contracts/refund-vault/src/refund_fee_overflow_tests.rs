//! Boundary test for `refund_fee`'s checked fee decomposition (issue #717).
//!
//! `refund_fee` used to compute `q * fee_bps + (r * fee_bps + 9_999) / 10_000`
//! with bare operators. With `[profile.release]` enabling `overflow-checks`
//! and `panic = "abort"`, a large amount aborted the host call instead of
//! returning its fee. The computation now runs through
//! `accensa_common::math::{mul_amounts, add_amounts, div_amounts}`; its
//! failure arm is the stated precondition `fee_bps <= 10_000`, enforced by
//! `set_fee_bps` and by `FLASH_LOAN_FEE_BPS`.
//!
//! The test drives `i128::MAX` at the maximum fee rate, where the old bare
//! product would have trapped.

use crate::refund_fee;

#[test]
fn full_fee_of_i128_max_does_not_trap() {
    // `q * 10_000 <= amount` and `r * 10_000` is a small constant, so the
    // decomposition returns the exact 100% fee at the type's maximum.
    assert_eq!(refund_fee(i128::MAX, 10_000), i128::MAX);
}

#[test]
fn sub_basis_point_fee_at_i128_max_rounds_up() {
    // `ceil((i128::MAX % 10_000) / 10_000)` is one unit: the remainder is
    // 5_727, so the fee is the whole part plus one.
    assert_eq!(refund_fee(i128::MAX, 1), i128::MAX / 10_000 + 1);
}

#[test]
fn rounding_contract_is_unchanged_at_small_values() {
    assert_eq!(refund_fee(1, 1), 1);
    assert_eq!(refund_fee(10_001, 1), 2);
    assert_eq!(refund_fee(10_000, 10_000), 10_000);
}
