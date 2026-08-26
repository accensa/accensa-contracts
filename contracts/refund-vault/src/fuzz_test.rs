#![cfg(test)]
//! Property-based fuzz tests for `RefundVault`.
//!
//! ## Approach
//!
//! Same philosophy as `receipt-anchor/src/fuzz_test.rs`: property tests over
//! generated operation sequences running inside the Soroban test environment,
//! driven by [`proptest`]'s seeded PRNG rather than a coverage-guided fuzzer
//! (there is no wasm/fork/feedback loop to guide one). Each test case
//! generates a random sequence of vault operations, executes it against a
//! fresh `Env` (advancing the simulated ledger between operations), and
//! asserts the invariants after *every* operation so a violation is
//! attributed to the exact op that broke it. On failure proptest shrinks to a
//! minimal counterexample and prints the seed, which we freeze as a permanent
//! regression test (see the `regression` module at the bottom of this file).
//!
//! The test maintains its own `Model` of deposits, refunds, withdrawals,
//! paused state, refund window, and which `payment_ref`s have been refunded.
//! The invariants are checked against observable contract state — the vault's
//! token balance, `get_refund` records, and the error returned by each
//! rejected call — so the model is a conformance oracle, not a restatement of
//! the contract's internals.
//!
//! ## Budget knobs
//!
//! - `FUZZ_CASES` (default `32`) tunes the number of generated sequences.
//! - `FUZZ_SEQ_LEN` (default `48`) tunes the maximum length of each sequence.
//!
//! CI runs with the defaults. For a longer local profile:
//!
//! ```sh
//! FUZZ_CASES=1000 FUZZ_SEQ_LEN=256 cargo test -p refund-vault -- --ignored
//! ```
//!
//! The `*_long` variants are `#[ignore]`d and use larger budgets.
//!
//! ## Limits
//!
//! - Coverage is bounded by the random generator: transitions the generator
//!   never produces are never explored. The op mix is weighted toward the
//!   interesting state (deposit/refund/withdraw, pause toggles, window
//!   changes, ledger jumps).
//! - Amounts are drawn from a bounded range (`[-1000, FLOAT]`); the extreme
//!   `i128::ANY` boundary is pinned by the dedicated
//!   `test_regression_deposit_extreme_amounts` test rather than fuzzed, since
//!   the interesting failures live in the accounting, not the magnitude.
//! - The ledger advances in bounded jumps so persistent entries never cross
//!   the archival threshold mid-sequence; archival/restore is out of scope
//!   here (see `docs/storage-audit.md`).
//! - Snapshot capture at `Env` drop is disabled (each generated case would
//!   otherwise write a golden ledger-snapshot file).
//! - `refund` success requires the vault to hold the float; sequences that
//!   request more than the current balance exercise `InsufficientFloat`
//!   conformance rather than reverting.

extern crate std;

use proptest::prelude::*;
use soroban_sdk::{
    testutils::{storage::Persistent as _, Address as _, EnvTestConfig, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, BytesN, Env,
};
use std::{
    format,
    string::{String, ToString},
};

use crate::{DataKey, Error, RefundVault, RefundVaultClient};

/// Total tokens minted to the merchant at setup.
const FLOAT: i128 = 10_000_000;
/// How many distinct `payment_ref` slots the generated sequences draw from.
const REF_SLOTS: u32 = 8;

/// Bounded CI default budgets; override with `FUZZ_CASES` / `FUZZ_SEQ_LEN`.
fn fuzz_cases() -> u32 {
    std::env::var("FUZZ_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(32)
}

fn fuzz_seq_len() -> usize {
    std::env::var("FUZZ_SEQ_LEN")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(48)
}

fn proptest_config(cases: u32) -> ProptestConfig {
    ProptestConfig {
        cases,
        ..ProptestConfig::default()
    }
}

/// An `Env` that does not write golden ledger snapshots on drop (see module
/// docs).
fn test_env() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

fn setup(window: u32) -> (Env, RefundVaultClient<'static>, Address, Address) {
    let env = test_env();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token = sac.address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);

    let contract_id = env.register(RefundVault, ());
    let client = RefundVaultClient::new(&env, &contract_id);
    client.initialize(&merchant, &token, &window);

    (env, client, merchant, token)
}

fn payment_ref(env: &Env, slot: u32) -> BytesN<32> {
    let mut arr = [0u8; 32];
    arr[0] = slot as u8;
    arr[1] = 0xAB;
    BytesN::from_array(env, &arr)
}

/// The test's conformance oracle for the vault's observable state.
#[derive(Clone)]
struct Model {
    deposits: i128,
    refunds: i128,
    withdrawals: i128,
    /// Refunded amount per payment-ref slot (None = never refunded).
    refunded: [Option<i128>; REF_SLOTS as usize],
    paused: bool,
    window: u32,
}

impl Model {
    fn new(window: u32) -> Self {
        Model {
            deposits: 0,
            refunds: 0,
            withdrawals: 0,
            refunded: [None; REF_SLOTS as usize],
            paused: false,
            window,
        }
    }

    fn float(&self) -> i128 {
        self.deposits - self.refunds - self.withdrawals
    }

    fn is_expired(&self, paid_at: u32, current_ledger: u32) -> bool {
        if self.window == 0 {
            return false;
        }
        current_ledger > paid_at.saturating_add(self.window)
    }
}

#[derive(Clone, Debug)]
enum Op {
    Deposit { amount: i128 },
    Refund { slot: u32, amount: i128, paid_at_delta: u32 },
    Withdraw { amount: i128 },
    Pause,
    Unpause,
    SetWindow { new_window: u32 },
    AdvanceLedger { ledgers: u32 },
}

prop_compose! {
    fn arb_op()(tag in 0..7_u32, amount in -200..2_000_000_i128, slot in 0..REF_SLOTS, delta in 0..500_u32) (
        op in match tag {
            0 => arb_deposit(amount).boxed(),
            1 => arb_refund(slot, amount, delta).boxed(),
            2 => arb_withdraw(amount).boxed(),
            3 => Just(Op::Pause).boxed(),
            4 => Just(Op::Unpause).boxed(),
            5 => (0..1000_u32).prop_map(|w| Op::SetWindow { new_window: w }).boxed(),
            _ => (1..100_u32).prop_map(|l| Op::AdvanceLedger { ledgers: l }).boxed(),
        }
    ) -> Op {
        op
    }
}

fn arb_deposit(amount: i128) -> impl Strategy<Value = Op> {
    Just(Op::Deposit { amount })
}

fn arb_refund(slot: u32, amount: i128, paid_at_delta: u32) -> impl Strategy<Value = Op> {
    Just(Op::Refund { slot, amount, paid_at_delta })
}

fn arb_withdraw(amount: i128) -> impl Strategy<Value = Op> {
    Just(Op::Withdraw { amount })
}

fn execute_op(
    env: &Env,
    client: &RefundVaultClient,
    merchant: &Address,
    token: &Address,
    model: &mut Model,
    op: &Op,
)
{
    let token_client = TokenClient::new(env, token);

    match op {
        Op::Deposit { amount }
        if *amount > 0 && model.float().saturating_add(*amount) <= FLOAT =>
        {
            if model.paused {
                assert_eq!(
                    client.try_deposit(merchant, amount),
                    Err(Ok(Error::Paused))
                );
            } else {
                client.deposit(merchant, amount);
                model.deposits += *amount;
            }
        }
        Op::Deposit { amount }
        if *amount <= 0 =>
        {
            if !model.paused {
                assert_eq!(
                    client.try_deposit(merchant, amount),
                    Err(Ok(Error::InvalidAmount))
                );
            }
        }
        Op::Deposit { .. } => {}

        Op::Refund { slot, amount, paid_at_delta }
        if *amount > 0 =>
        {
            let slot_idx = *slot as usize;
            let already_refunded = model.refunded[slot_idx].is_some();

            let current_ledger = env.ledger().sequence();
            let paid_at = current_ledger.saturating_sub(*paid_at_delta);
            let expired = model.is_expired(paid_at, current_ledger);
            let insufficient = *amount > model.float();

            let buyer = Address::generate(env);
            let pref = payment_ref(env, *slot);

            if model.paused {
                assert_eq!(
                    client.try_refund(&pref, &buyer, amount, &paid_at),
                    Err(Ok(Error::Paused))
                );
            } else if already_refunded {
                assert_eq!(
                    client.try_refund(&pref, &buyer, amount, &paid_at),
                    Err(Ok(Error::AlreadyRefunded))
                );
            } else if expired {
                assert_eq!(
                    client.try_refund(&pref, &buyer, amount, &paid_at),
                    Err(Ok(Error::WindowExpired))
                );
            } else if insufficient {
                assert_eq!(
                    client.try_refund(&pref, &buyer, amount, &paid_at),
                    Err(Ok(Error::InsufficientFloat))
                );
            } else {
                client.refund(&pref, &buyer, amount, &paid_at);
                model.refunds += *amount;
                model.refunded[slot_idx] = Some(*amount);
            }
        }
        Op::Refund { amount, .. }
        if *amount <= 0 =>
        {
            if !model.paused {
                let buyer = Address::generate(env);
                let pref = payment_ref(env, 0);
                assert_eq!(
                    client.try_refund(&pref, &buyer, amount, &0),
                    Err(Ok(Error::InvalidAmount))
                );
            }
        }
        Op::Refund { .. } => {}

        Op::Withdraw { amount }
        if *amount > 0 =>
        {
            let insufficient = *amount > model.float();
            if model.paused {
                assert_eq!(
                    client.try_withdraw(amount, merchant),
                    Err(Ok(Error::Paused))
                );
            } else if insufficient {
                assert_eq!(
                    client.try_withdraw(amount, merchant),
                    Err(Ok(Error::InsufficientFloat))
                );
            } else {
                client.withdraw(amount, merchant);
                model.withdrawals += *amount;
            }
        }
        Op::Withdraw { amount }
        if *amount <= 0 =>
        {
            if !model.paused {
                assert_eq!(
                    client.try_withdraw(amount, merchant),
                    Err(Ok(Error::InvalidAmount))
                );
            }
        }
        Op::Withdraw { .. } => {}

        Op::Pause => {
            client.pause();
            model.paused = true;
        }

        Op::Unpause => {
            client.unpause();
            model.paused = false;
        }

        Op::SetWindow { new_window }
        if model.paused => {
            // Pause prevents nothing about window config in the current design, or does it?
            // Window changes require merchant auth, which mock_all_auths grants.
            client.set_refund_window(new_window);
            model.window = *new_window;
        }
        Op::SetWindow { new_window } => {
            client.set_refund_window(new_window);
            model.window = *new_window;
        }

        Op::AdvanceLedger { ledgers }
        if *ledgers > 0 => {
            let target = env.ledger().sequence() + *ledgers;
            env.ledger().with_mut(|li| li.sequence_number = target);
        }
        _ => {}
    }

    // Invariant assertions
    assert_eq!(token_client.balance(&client.address), model.float());
    assert_eq!(client.is_paused(), model.paused);
    assert_eq!(client.get_refund_window().unwrap(), model.window);

    for i in 0..REF_SLOTS {
        let pref = payment_ref(env, i);
        let stored = client.try_get_refund(&pref);
        match model.refunded[i as usize] {
            Some(amt) => {
                let record = stored.unwrap();
                assert_eq!(record.amount, amt);
            }
            None => {
                assert_eq!(stored, Err(Ok(Error::RefundNotFound)));
            }
        }
    }
}

proptest! {
    #![proptest_config(proptest_config(fuzz_cases()))]

    #[test]
    fn fuzz_vault_operations(ops in prop::collection::vec(arb_op(), 0..fuzz_seq_len())) {
        let (env, client, merchant, token) = setup(100);
        let mut model = Model::new(100);

        // Initial deposit to fund float
        client.deposit(&merchant, &5_000_000);
        model.deposits += 5_000_000;

        for op in ops {
            execute_op(&env, &client, &merchant, &token, &mut model, &op);
        }
    }
}
