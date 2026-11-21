//! Arithmetic table for `calculate_slashing_penalty` (issue #987).
//!
//! Pins the helper's truncation-toward-zero rounding policy at the in-range
//! percentages 0, 1, 50 and 100, an odd amount that truncates, and the zero
//! amount. The helper is called at the crate root so the table proves the
//! re-export path, not a copied formula.
//!
//! A local trait maps the return value to `i128` whether the helper reports
//! in-range arithmetic as a plain value or as a `Result`, so the table stays
//! valid regardless of how #986-shaped changes surface invalid input.

use refund_policy_vdf::calculate_slashing_penalty;
use soroban_sdk::Env;

trait PenaltyValue {
    fn penalty(self) -> i128;
}

impl PenaltyValue for i128 {
    fn penalty(self) -> i128 {
        self
    }
}

impl PenaltyValue for Result<i128, accensa_common::Error> {
    fn penalty(self) -> i128 {
        self.expect("in-range (amount, penalty) must succeed")
    }
}

#[test]
fn slashing_penalty_table() {
    let env = Env::default();

    // (amount, penalty, expected)
    let table: [(i128, u32, i128); 9] = [
        (0, 0, 0),
        (0, 1, 0),
        (0, 50, 0),
        (0, 100, 0),
        (1000, 0, 0),
        (1000, 1, 10),
        (1000, 50, 500),
        (1000, 100, 1000),
        // Odd amount, odd percentage — (999 * 33) / 100 = 329.67 truncates to
        // 329; this is the whole rounding policy of the helper.
        (999, 33, 329),
    ];

    for (amount, penalty, expected) in table {
        let got = calculate_slashing_penalty(&env, amount, penalty).penalty();
        assert_eq!(
            got, expected,
            "amount={amount}, penalty={penalty}: expected {expected}, got {got}"
        );
    }
}
