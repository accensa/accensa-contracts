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
//! paused state, refund window, and per-`payment_ref` cumulative refunds and
//! ceilings. The invariants are checked against observable contract state —
//! the vault's token balance, `get_refund` records, and the error returned by
//! each rejected call — so the model is a conformance oracle, not a
//! restatement of the contract's internals.
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
    vec::Vec,
};

use crate::test_helpers::vault_init;
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

    let contract_id = env.register(RefundVault, (vault_init(&env, &merchant, &token, window),));
    let client = RefundVaultClient::new(&env, &contract_id);

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
    /// Per payment-ref slot: (cumulative refunded, ceiling). `None` = never
    /// refunded. The ceiling is fixed by the first partial and never changes,
    /// mirroring the contract (later calls' `payment_amount` is ignored once a
    /// record exists).
    refunded: [Option<(i128, i128)>; REF_SLOTS as usize],
    paused: bool,
    window: u32,
    merchant_balance: i128,
    /// Per-user (merchant) nonce: the next nonce a successful refund op must
    /// present. Mitigates replay attacks on the signed refund RPC (issue #122).
    refund_nonce: u64,
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
            merchant_balance: FLOAT,
            refund_nonce: 0,
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

    // Invariant Test (#94): RefundVault's total internal token balance MUST equal
    // sum of all recorded individual user claims/liabilities (Total Deposits - Total Refunds - Total Withdrawals).
    proptest::proptest! {
        fn test_fuzz_refund_vault_balance_invariant(
            deposit_amounts in proptest::collection::vec(1i128..10_000_000i128, 1..5),
            refund_amounts in proptest::collection::vec(1i128..1_000_000i128, 1..5),
            withdraw_amounts in proptest::collection::vec(1i128..1_000_000i128, 1..5)
        ) {
            let (env, client, merchant, token) = setup(100);
            let token_client = soroban_sdk::token::Client::new(&env, &token);

            let mut expected_balance: i128 = 0;

            // Deposits
            for amt in deposit_amounts {
                if client.try_deposit(&merchant, &amt).is_ok() {
                    expected_balance += amt;
                }
                let actual_balance = token_client.balance(&client.address);
                assert_eq!(actual_balance, expected_balance, "Invariant mismatch after deposit");
            }

            // Refunds
            let mut refund_nonce: u64 = 0;
            for (idx, amt) in refund_amounts.into_iter().enumerate() {
                let mut ref_bytes = [0u8; 32];
                ref_bytes[0] = (idx + 1) as u8;
                let payment_ref = BytesN::from_array(&env, &ref_bytes);
                let recipient = Address::generate(&env);

                if client
                    .try_refund(&payment_ref, &recipient, &amt, &0, &amt, &None, &refund_nonce)
                    .is_ok()
                {
                    expected_balance -= amt;
                    refund_nonce += 1;
                }
                let actual_balance = token_client.balance(&client.address);
                assert_eq!(actual_balance, expected_balance, "Invariant mismatch after refund");
            }

            // Withdrawals
            for amt in withdraw_amounts {
                if client.try_withdraw(&amt, &merchant).is_ok() {
                    expected_balance -= amt;
                }
                let actual_balance = token_client.balance(&client.address);
                assert_eq!(actual_balance, expected_balance, "Invariant mismatch after withdrawal");
            }
        }
    }
}

/// Headroom percentage (15%) chosen to account for minor toolchain/host optimization differences.
const HEADROOM_PERCENT: u64 = 15;

/// Cost baselines for `RefundVault::refund`
/// Measured via `env.cost_estimate().budget().cpu_instruction_cost()` and `env.cost_estimate().budget().memory_bytes_cost()` on 2026-08-30.
/// Re-baselined after the partial-refund, TTL-guard, reentrancy-guard,
/// oracle-policy and commit-reveal additions grew the `refund` path (see
/// `docs/RELEASING.md` re-baselining procedure; measured with the oracle
/// policy *unset* but after the commit-reveal lookup hooked into `claim_single`).
const REFUND_BASELINE_CPU: u64 = 704_621;
const REFUND_BASELINE_MEM: u64 = 197_217;

#[test]
fn test_refund_resource_cost_budget() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &1_000_000);

    let payment_ref = BytesN::from_array(&env, &[1u8; 32]);
    let recipient = Address::generate(&env);

    env.cost_estimate().budget().reset_default();
    client.refund(&payment_ref, &recipient, &100_000, &0, &100_000, &None, &0);
    let cpu_refund = env.cost_estimate().budget().cpu_instruction_cost();
    let mem_refund = env.cost_estimate().budget().memory_bytes_cost();

    let max_cpu_refund = REFUND_BASELINE_CPU + (REFUND_BASELINE_CPU * HEADROOM_PERCENT / 100);
    let max_mem_refund = REFUND_BASELINE_MEM + (REFUND_BASELINE_MEM * HEADROOM_PERCENT / 100);

    assert!(
        cpu_refund <= max_cpu_refund,
        "RefundVault::refund CPU cost regression! Function: refund, Limit: {}, Measured: {}",
        max_cpu_refund,
        cpu_refund
    );
    assert!(
        mem_refund <= max_mem_refund,
        "RefundVault::refund Memory cost regression! Function: refund, Limit: {}, Measured: {}",
        max_mem_refund,
        mem_refund
    );
}

#[derive(Clone, Debug)]
enum Op {
    Deposit {
        amount: i128,
    },
    Refund {
        slot: u32,
        amount: i128,
        paid_at_ledger: u32,
        payment_amount: i128,
    },
    Withdraw {
        amount: i128,
    },
    SetWindow {
        window: u32,
    },
    TogglePause,
    Advance {
        ledgers: u32,
    },
    ExtendTtl {
        slot: u32,
    },
}

fn amount_strategy() -> impl Strategy<Value = i128> {
    (-1000i128..=FLOAT).boxed()
}

fn arb_op() -> impl Strategy<Value = Op> {
    op_strategy()
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        amount_strategy().prop_map(|amount| Op::Deposit { amount }),
        (
            0u32..REF_SLOTS,
            amount_strategy(),
            0u32..=1_000_000u32,
            amount_strategy()
        )
            .prop_map(
                |(slot, amount, paid_at_ledger, payment_amount)| Op::Refund {
                    slot,
                    amount,
                    paid_at_ledger,
                    payment_amount,
                }
            ),
        amount_strategy().prop_map(|amount| Op::Withdraw { amount }),
        (0u32..=5_000u32).prop_map(|window| Op::SetWindow { window }),
        Just(Op::TogglePause),
        (1u32..=20u32).prop_map(|ledgers| Op::Advance { ledgers }),
        (0u32..REF_SLOTS).prop_map(|slot| Op::ExtendTtl { slot }),
    ]
}

fn execute_op(
    env: &Env,
    client: &RefundVaultClient,
    merchant: &Address,
    token: &Address,
    model: &mut Model,
    op: &Op,
) -> Vec<String> {
    let token_client = TokenClient::new(env, token);
    let balance = || token_client.balance(&client.address);
    let mut failures: Vec<String> = Vec::new();

    match op {
        Op::Deposit { amount } => {
            let res = client.try_deposit(merchant, amount);
            match &res {
                Ok(Ok(())) => {
                    if *amount <= 0 {
                        failures.push(format!("deposit of {amount} succeeded but must be invalid"));
                    } else if *amount > model.merchant_balance {
                        failures.push(format!(
                            "deposit of {amount} succeeded beyond merchant balance {}",
                            model.merchant_balance
                        ));
                    } else {
                        model.deposits += *amount;
                        model.merchant_balance -= *amount;
                    }
                }
                Err(Ok(Error::Paused)) => {
                    if !model.paused {
                        failures.push("deposit returned Paused while unpaused".to_string());
                    }
                }
                Err(Ok(Error::InvalidAmount)) => {
                    if *amount > 0 {
                        failures.push(format!("deposit of {amount} rejected as invalid amount"));
                    }
                }
                Err(_) | Ok(Err(_)) => {
                    // Transfer rejected by the token contract (insufficient
                    // merchant balance) surfaces as a host error; allow it.
                }
            }
            if balance() != model.float() {
                failures.push(format!(
                    "float {} != model {} after Deposit({amount})",
                    balance(),
                    model.float()
                ));
            }
        }
        Op::Refund {
            slot,
            amount,
            paid_at_ledger,
            payment_amount,
        } => {
            let idx = *slot as usize;
            let before = balance();
            let res = client.try_refund(
                &payment_ref(env, *slot),
                merchant,
                amount,
                paid_at_ledger,
                payment_amount,
                &None,
                &model.refund_nonce,
            );
            let (cumulative, ceiling) = model.refunded[idx].unwrap_or((0, *payment_amount));
            match res {
                Ok(Ok(())) => {
                    if *amount <= 0 {
                        failures.push(format!("refund of {amount} succeeded but must be invalid"));
                    } else if model.window > 0
                        && env.ledger().sequence() > paid_at_ledger + model.window
                    {
                        failures.push(format!(
                            "refund past the window succeeded (ledger {}, paid at {paid_at_ledger}, window {})",
                            env.ledger().sequence(),
                            model.window
                        ));
                    } else if cumulative.checked_add(*amount).is_none()
                        || cumulative + *amount > ceiling
                    {
                        failures.push(format!(
                            "refund of {amount} succeeded past the ceiling {ceiling} "
                        ));
                    } else if *amount > model.float() {
                        failures.push(format!(
                            "refund of {amount} succeeded beyond float {}",
                            model.float()
                        ));
                    } else {
                        model.refunds += *amount;
                        model.merchant_balance += *amount;
                        model.refunded[idx] = Some((cumulative + *amount, ceiling));
                        model.refund_nonce += 1;
                    }
                }
                Err(Ok(Error::Paused)) => {
                    if !model.paused {
                        failures.push("refund returned Paused while unpaused".to_string());
                    }
                }
                Err(Ok(Error::ExceedsPayment)) => {
                    let over_ceiling =
                        cumulative.checked_add(*amount).is_none() || cumulative + *amount > ceiling;
                    if !over_ceiling {
                        failures.push(format!(
                            "refund of slot {slot} rejected as ExceedsPayment but cumulative \
                             {cumulative} + {amount} <= ceiling {ceiling}"
                        ));
                    }
                }
                Err(Ok(Error::WindowExpired)) => {
                    let expired =
                        model.window > 0 && env.ledger().sequence() > paid_at_ledger + model.window;
                    if !expired {
                        failures.push(format!(
                            "refund rejected as WindowExpired but ledger {} <= paid {} + window {}",
                            env.ledger().sequence(),
                            paid_at_ledger,
                            model.window
                        ));
                    }
                }
                Err(Ok(Error::InsufficientFloat)) => {
                    if *amount <= model.float() {
                        failures.push(format!(
                            "refund of {amount} rejected as InsufficientFloat with float {}",
                            model.float()
                        ));
                    }
                }
                Err(Ok(Error::StaleState)) => {
                    // Reaching the refund path with the wrong nonce means the
                    // model lost track of successful refunds (issue #122).
                    failures.push(format!(
                        "refund of slot {slot} rejected as StaleState with nonce {}",
                        model.refund_nonce
                    ));
                }
                Err(Ok(Error::InvalidAmount)) => {
                    if *amount > 0 {
                        failures.push(format!("refund of {amount} rejected as invalid amount"));
                    }
                }
                Err(Err(_)) => {
                    failures.push("refund returned an unexpected host error".to_string());
                }
                Ok(Err(_)) => {
                    failures.push(format!(
                        "refund of slot {slot} failed to convert its result"
                    ));
                }
                Err(Ok(_)) => {
                    failures.push("refund returned an unexpected error".to_string());
                }
            }
            if balance() != model.float() {
                failures.push(format!(
                    "float {} != model {} after Refund({slot}, {amount})",
                    balance(),
                    model.float()
                ));
            }
            let record = client.get_refund(&payment_ref(env, *slot));
            match (record, model.refunded[idx]) {
                (Some(r), Some((cum, ceiling))) => {
                    if r.amount_refunded != cum {
                        failures.push(format!(
                            "get_refund cumulative {} != modelled {cum} for slot {slot}",
                            r.amount_refunded
                        ));
                    }
                    if r.payment_amount != ceiling {
                        failures.push(format!(
                            "get_refund ceiling {} != modelled {ceiling} for slot {slot}",
                            r.payment_amount
                        ));
                    }
                }
                (Some(_), None) => failures.push(format!(
                    "get_refund returned a record for never-refunded slot {slot}"
                )),
                (None, Some(_)) => {
                    failures.push(format!("get_refund missing for refunded slot {slot}"))
                }
                (None, None) => {}
            }
            if balance() != before && model.paused {
                failures.push(format!(
                    "balance changed while paused during Refund({slot}, {amount})"
                ));
            }
        }
        Op::Withdraw { amount } => {
            let before = balance();
            let res = client.try_withdraw(amount, merchant);
            match res {
                Ok(Ok(())) => {
                    if *amount <= 0 {
                        failures.push(format!(
                            "withdraw of {amount} succeeded but must be invalid"
                        ));
                    } else if *amount > model.float() {
                        failures.push(format!(
                            "withdraw of {amount} succeeded beyond float {}",
                            model.float()
                        ));
                    } else {
                        model.withdrawals += *amount;
                        model.merchant_balance += *amount;
                    }
                }
                Err(Ok(Error::Paused)) => {
                    if !model.paused {
                        failures.push("withdraw returned Paused while unpaused".to_string());
                    }
                }
                Err(Ok(Error::InvalidAmount)) => {
                    if *amount > 0 {
                        failures.push(format!("withdraw of {amount} rejected as invalid amount"));
                    }
                }
                Err(Ok(Error::InsufficientFloat)) => {
                    if *amount <= model.float() {
                        failures.push(format!(
                            "withdraw of {amount} rejected as InsufficientFloat with float {}",
                            model.float()
                        ));
                    }
                }
                Err(Err(_)) => {
                    failures.push("withdraw returned an unexpected host error".to_string());
                }
                Ok(Err(_)) => {
                    failures.push(format!("withdraw of {amount} failed to convert its result"));
                }
                Err(Ok(_)) => {
                    failures.push("withdraw returned an unexpected error".to_string());
                }
            }
            if balance() != model.float() {
                failures.push(format!(
                    "float {} != model {} after Withdraw({amount})",
                    balance(),
                    model.float()
                ));
            }
            if balance() != before && model.paused {
                failures.push(format!(
                    "balance changed while paused during Withdraw({amount})"
                ));
            }
        }
        Op::SetWindow { window } => {
            // The refund window is only changeable via propose_policy +
            // execute_policy (the timelock must elapse first). Emulate that
            // flow so the model's window tracks the contract's real window.
            let _ = client.try_propose_policy(window, &0, &0);
            let seq = env.ledger().sequence();
            env.ledger()
                .with_mut(|li| li.sequence_number = seq + crate::POLICY_TIMELOCK);
            if client.try_execute_policy() == Ok(Ok(())) {
                model.window = *window;
            }
        }
        Op::TogglePause => {
            if model.paused {
                client.unpause();
            } else {
                client.pause();
            }
            model.paused = !model.paused;
        }
        Op::Advance { ledgers } => {
            env.ledger().with_mut(|li| li.sequence_number += ledgers);
        }
        Op::ExtendTtl { slot } => {
            let ref_ = payment_ref(env, *slot);
            let idx = *slot as usize;
            if model.refunded[idx].is_none() {
                let res = client.try_extend_refund_ttl(&ref_);
                if res != Err(Ok(Error::RefundNotFound)) {
                    failures.push(format!(
                        "extend_refund_ttl on unrefunded slot {slot}: expected RefundNotFound, got {res:?}"
                    ));
                }
            } else {
                let ttl_before = env.as_contract(&client.address, || {
                    env.storage()
                        .persistent()
                        .get_ttl(&DataKey::RefundV2(ref_.clone()))
                });
                client.extend_refund_ttl(&ref_);
                let ttl_after = env.as_contract(&client.address, || {
                    env.storage()
                        .persistent()
                        .get_ttl(&DataKey::RefundV2(ref_.clone()))
                });
                if ttl_after < ttl_before {
                    failures.push(format!(
                        "extend_refund_ttl shortened TTL of slot {slot}: {ttl_before} -> {ttl_after}"
                    ));
                }
            }
        }
    }

    failures
}

fn execute(
    env: &Env,
    client: &RefundVaultClient,
    merchant: &Address,
    token: &Address,
    ops: &[Op],
) -> Vec<String> {
    let mut model = Model::new(100);
    let mut failures = Vec::new();
    for op in ops {
        failures.extend(execute_op(env, client, merchant, token, &mut model, op));
        if !failures.is_empty() {
            break;
        }
    }
    failures
}

proptest! {
    #![proptest_config(proptest_config(fuzz_cases()))]

    #[test]
    fn test_fuzz_refund_ceiling_respected(
        ops in proptest::collection::vec(op_strategy(), 0..=fuzz_seq_len()),
    ) {
        let (env, client, merchant, token) = setup(100);
        let failures = execute(&env, &client, &merchant, &token, &ops);
        assert!(
            failures.is_empty(),
            "refund-ceiling invariants violated:\n{}",
            failures.join("\n")
        );
    }
}

proptest! {
    #![proptest_config(proptest_config(fuzz_cases()))]

    #[test]
    fn fuzz_vault_operations(ops in prop::collection::vec(arb_op(), 0..fuzz_seq_len())) {
        let (env, client, merchant, token) = setup(100);
        let failures = execute(&env, &client, &merchant, &token, &ops);
        assert!(
            failures.is_empty(),
            "fuzz invariants violated:\n{}",
            failures.join("\n")
        );
    }

    /// TTL extension on a refund record never shortens its TTL; extension on
    /// a missing record always errors with RefundNotFound.
    #[test]
    fn test_fuzz_ttl_extension(
        missing_slot in 0u32..REF_SLOTS,
        advances in proptest::collection::vec(1u32..=1500u32, 1..=8),
    ) {
        let (env, client, merchant, _token) = setup(100);
        // Seed a refund record so there is a TTL to extend.
        client.deposit(&merchant, &1_000_000);
        let buyer = Address::generate(&env);
        let ref_ = payment_ref(&env, 0);
        client.refund(&ref_, &buyer, &100_000, &0, &100_000, &None, &0);

        // Extension on a record that does not exist errors. Slot 0 is the
        // refunded one, so pick a guaranteed-distinct slot (1..REF_SLOTS).
        let missing = payment_ref(&env, ((missing_slot + 1) % (REF_SLOTS - 1)) + 1);
        assert_eq!(
            client.try_extend_refund_ttl(&missing),
            Err(Ok(Error::RefundNotFound))
        );

        // Extension never shortens the record's TTL, and the record stays
        // readable after each extension.
        for advance in advances {
            env.ledger().with_mut(|li| li.sequence_number += advance);
            let ttl_before = env.as_contract(&client.address, || {
                env.storage()
                    .persistent()
                    .get_ttl(&DataKey::RefundV2(ref_.clone()))
            });
            client.extend_refund_ttl(&ref_);
            let ttl_after = env.as_contract(&client.address, || {
                env.storage()
                    .persistent()
                    .get_ttl(&DataKey::RefundV2(ref_.clone()))
            });
            assert!(
                ttl_after >= ttl_before,
                "extend_refund_ttl shortened TTL: {ttl_before} -> {ttl_after}"
            );
            assert!(client.get_refund(&ref_).is_some());
        }
    }
}

// ── Long local profile ──────────────────────────────────────────────────────
//
// Run with: cargo test -p refund-vault -- --ignored
// For an even longer run: FUZZ_CASES=2000 FUZZ_SEQ_LEN=256 cargo test -p
// refund-vault fuzz_test::test_fuzz_float_accounting_long -- --ignored

proptest! {
    #![proptest_config(proptest_config(128))]

    #[ignore]
    #[test]
    fn test_fuzz_float_accounting_long(
        ops in proptest::collection::vec(op_strategy(), 0..=128),
    ) {
        let (env, client, merchant, token) = setup(100);
        let failures = execute(&env, &client, &merchant, &token, &ops);
        assert!(
            failures.is_empty(),
            "float accounting invariants violated:\n{}",
            failures.join("\n")
        );
    }

    #[test]
    fn test_fuzz_refund_i128_boundaries(
        amount in prop_oneof![
            Just(0i128),
            Just(1i128),
            Just(-1i128),
            Just(i128::MIN),
            Just(i128::MIN + 1),
            Just(i128::MAX),
            Just(i128::MAX - 1),
            proptest::num::i128::ANY,
        ]
    ) {
        let (env, client, merchant, _token) =
            setup(100);
        client.deposit(&merchant, &100);

        let payment_ref =
            BytesN::from_array(&env, &[0u8; 32]);
        let buyer = Address::generate(&env);
        let res = client.try_refund(
            &payment_ref, &buyer, &amount, &0, &amount, &None, &0,
        );

        if amount <= 0 {
            assert_eq!(
                res, Err(Ok(Error::InvalidAmount))
            );
        } else if amount > 100 {
            assert_eq!(
                res,
                Err(Ok(Error::InsufficientFloat))
            );
        } else {
            assert!(res.is_ok());
        }
    }

    #[test]
    fn test_fuzz_deposit_i128_boundaries(
        amount in prop_oneof![
            Just(0i128),
            Just(1i128),
            Just(-1i128),
            Just(i128::MIN),
            Just(i128::MIN + 1),
            Just(i128::MAX),
            Just(i128::MAX - 1),
            proptest::num::i128::ANY,
        ]
    ) {
        let (_, client, merchant, _) = setup(100);
        let res = client.try_deposit(
            &merchant, &amount,
        );

        if amount <= 0 {
            assert_eq!(
                res, Err(Ok(Error::InvalidAmount))
            );
        } else if amount > FLOAT {
            assert!(res.is_err());
        } else {
            assert!(res.is_ok());
        }
    }
}

// ── Accounting invariant fuzz test ─────────────────────────────────────────

#[derive(Debug, Clone)]
enum VaultOp {
    Deposit(i128),
    Refund(i128),
    Withdraw(i128),
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    #[test]
    fn test_fuzz_vault_accounting_invariant(
        ops in prop::collection::vec(
            prop_oneof![
                (1i128..100_000).prop_map(VaultOp::Deposit),
                (1i128..1_000).prop_map(VaultOp::Refund),
                (1i128..1_000).prop_map(VaultOp::Withdraw),
            ],
            0..30,
        )
    ) {
        let (env, client, merchant, token) =
            setup(100_000_000);
        let token_client = TokenClient::new(
            &env, &token,
        );

        let mut total_deposits: i128 = 0;
        let mut total_refunds: i128 = 0;
        let mut total_withdrawals: i128 = 0;
        let mut refund_counter: u32 = 0;
        let mut refund_nonce: u64 = 0;

        for op in ops {
            match op {
                VaultOp::Deposit(amount) => {
                    if token_client.balance(&merchant)
                        >= amount
                        && client
                            .try_deposit(&merchant, &amount)
                            .is_ok()
                    {
                        total_deposits += amount;
                    }
                }
                VaultOp::Refund(amount) => {
                    let mut pr = [0u8; 32];
                    pr[..4].copy_from_slice(
                        &refund_counter.to_le_bytes(),
                    );
                    refund_counter = refund_counter
                        .wrapping_add(1);
                    let payment_ref =
                        BytesN::from_array(&env, &pr);
                    let buyer =
                        Address::generate(&env);
                    if client
                        .try_refund(
                            &payment_ref,
                            &buyer,
                            &amount,
                            &0,
                            &amount,
                            &None,
                            &refund_nonce,
                        )
                        .is_ok()
                    {
                        total_refunds += amount;
                        refund_nonce += 1;
                    }
                }
                VaultOp::Withdraw(amount) => {
                    if client
                        .try_withdraw(
                            &amount, &merchant,
                        )
                        .is_ok()
                    {
                        total_withdrawals += amount;
                    }
                }
            }
        }

        let vault_balance = token_client
            .balance(&client.address);

        // Invariant 1: vault float is non-negative.
        prop_assert!(
            vault_balance >= 0,
            "vault balance must be >= 0, got {}",
            vault_balance,
        );

        // Invariant 2: without yield, vault balance
        // equals net flow through the contract.
        prop_assert_eq!(
            vault_balance,
            total_deposits
                - total_refunds
                - total_withdrawals,
            "vault balance ({}) must equal \
             deposits ({}) - refunds ({}) \
             - withdrawals ({})",
            vault_balance,
            total_deposits,
            total_refunds,
            total_withdrawals,
        );
    }
}

// ── Regression corpus ──────────────────────────────────────────────────────
//
// Any failure found by the property tests above is frozen here as a permanent
// deterministic example, per the issue's seed-corpus requirement.

#[test]
fn test_regression_deposit_extreme_amounts() {
    // The i128 boundary previously fuzzed standalone: negative and zero
    // amounts are rejected as InvalidAmount; amounts beyond the minted float
    // fail in the token contract; in-range amounts succeed and move exactly
    // that much into the vault.
    let (env, client, merchant, token) = setup(100);
    let token_client = TokenClient::new(&env, &token);

    assert_eq!(
        client.try_deposit(&merchant, &-1),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        client.try_deposit(&merchant, &0),
        Err(Ok(Error::InvalidAmount))
    );

    client.deposit(&merchant, &5_000_000);
    assert_eq!(token_client.balance(&client.address), 5_000_000);

    // Beyond the merchant's remaining balance: the SAC transfer aborts.
    assert!(client.try_deposit(&merchant, &6_000_000).is_err());
    assert_eq!(token_client.balance(&client.address), 5_000_000);
}

#[test]
fn test_regression_float_accounts_across_full_cycle() {
    let (env, client, merchant, token) = setup(100);
    let token_client = TokenClient::new(&env, &token);

    client.deposit(&merchant, &1_000_000);
    client.deposit(&merchant, &2_000_000);
    assert_eq!(token_client.balance(&client.address), 3_000_000);

    let ref_a = payment_ref(&env, 0);
    let buyer = Address::generate(&env);
    client.refund(&ref_a, &buyer, &400_000, &0, &400_000, &None, &0);
    assert_eq!(token_client.balance(&client.address), 2_600_000);

    client.withdraw(&500_000, &merchant);
    assert_eq!(token_client.balance(&client.address), 2_100_000);

    // The ceiling guard holds even after other activity: cumulative 400_000
    // + 100 would exceed the 400_000 ceiling.
    assert_eq!(
        client.try_refund(&ref_a, &buyer, &100, &0, &400_000, &None, &1),
        Err(Ok(Error::ExceedsPayment))
    );
    assert_eq!(token_client.balance(&client.address), 2_100_000);
}

#[test]
fn test_regression_pause_blocks_and_preserves_state() {
    let (env, client, merchant, token) = setup(100);
    let token_client = TokenClient::new(&env, &token);

    client.deposit(&merchant, &1_000_000);
    client.pause();

    assert_eq!(client.try_deposit(&merchant, &100), Err(Ok(Error::Paused)));
    assert_eq!(client.try_withdraw(&100, &merchant), Err(Ok(Error::Paused)));

    let buyer = Address::generate(&env);
    let ref_ = payment_ref(&env, 1);
    assert_eq!(
        client.try_refund(&ref_, &buyer, &100, &0, &100, &None, &0),
        Err(Ok(Error::Paused))
    );
    assert!(client.get_refund(&ref_).is_none());
    assert_eq!(token_client.balance(&client.address), 1_000_000);

    client.unpause();
    client.refund(&ref_, &buyer, &100, &0, &100, &None, &0);
    assert_eq!(token_client.balance(&client.address), 999_900);
}
