//! Governance as `ReceiptAnchor`'s admin — closes the single-admin-key SPOF
//! (see `docs/SECURITY_MODEL.md`) for the merchant-gated calls that
//! genuinely exist on the contract (`set_anchor_rate_limit`,
//! `anchor_batch`, `prune_batches`). `ReceiptAnchor` has no upgrade entry
//! point (see `docs/ADR-003-upgradeability.md`, accepted: both contracts are
//! deliberately immutable), so there is nothing to gate there — this
//! exercises the admin surface that actually exists.
//!
//! `ReceiptAnchor::initialize` accepts any `Address` as merchant, including
//! a contract's (`multisig_admin_anchor.rs` already proves this with a
//! `MultisigAccount`). These tests initialize it with a `Governance`
//! instance instead and drive `set_anchor_rate_limit`/`prune_batches`
//! through `propose`/`vote`/`execute`, with **no change to `ReceiptAnchor`
//! itself**: `execute`'s nested call satisfies `merchant.require_auth()`
//! because the host auto-authorizes a contract's own address when that
//! contract is the direct caller — the same mechanism `ReceiptAnchor`
//! already relies on for its `MultisigAccount` admin.
//!
//! No `mock_all_auths()`: member votes use real, explicit auth entries
//! (`mock_auths`) so the tests prove a specific weighted quorum is required,
//! and `execute` itself runs with **no auth entries at all** — proving the
//! self-authorization path is real, not a testing artifact.

use governance::{Governance, GovernanceClient};
use receipt_anchor::{ReceiptAnchor, ReceiptAnchorClient};
use soroban_sdk::{
    testutils::{Address as _, MockAuth, MockAuthInvoke},
    Address, BytesN, Env, IntoVal, Symbol, Vec,
};

/// Logical shard used by these governance-admin tests (single-stream).
const DEFAULT_SHARD: u64 = 0;

/// The `ReceiptShard` wasm, built by `cargo build -p receipt-shard --target
/// wasm32v1-none --release` before these tests run (see `.github/workflows/ci.yml`).
mod shard_wasm {
    soroban_sdk::contractimport!(file = "../../target/wasm32v1-none/release/receipt_shard.wasm");
}

/// Two members, weighted 1/1, 100% quorum (both must vote yes), a
/// 1000-ledger voting window. `ReceiptAnchor` is initialized with the
/// governance contract as merchant.
fn setup() -> (
    Env,
    GovernanceClient<'static>,
    ReceiptAnchorClient<'static>,
    Address,
    Address,
    Address,
) {
    let env = Env::default();

    let m1 = Address::generate(&env);
    let m2 = Address::generate(&env);

    let members = Vec::from_array(&env, [m1.clone(), m2.clone()]);
    let weights = Vec::from_array(&env, [1u64, 1u64]);
    let gov_id = env.register(Governance, (members, weights, 10_000u32, 1000u32));
    let gov = GovernanceClient::new(&env, &gov_id);

    let anchor_id = env.register(ReceiptAnchor, ());
    let anchor = ReceiptAnchorClient::new(&env, &anchor_id);
    // `initialize` is not auth-gated, so it needs no auth entries.
    let shard_wasm_hash = env.deployer().upload_contract_wasm(shard_wasm::WASM);
    anchor.initialize(&gov_id.clone(), &shard_wasm_hash);

    (env, gov, anchor, gov_id, m1, m2)
}

/// Mock a real auth entry for `voter` calling `gov::vote(voter, proposal_id, support)`.
fn mock_vote_auth(env: &Env, gov_id: &Address, voter: &Address, proposal_id: u64, support: bool) {
    let args = soroban_sdk::vec![
        env,
        voter.into_val(env),
        proposal_id.into_val(env),
        support.into_val(env),
    ];
    env.mock_auths(&[MockAuth {
        address: voter,
        invoke: &MockAuthInvoke {
            contract: gov_id,
            fn_name: "vote",
            args,
            sub_invokes: &[],
        },
    }]);
}

/// Mock a real auth entry for `proposer` calling
/// `gov::propose(proposer, target, function, args)`.
fn mock_propose_auth(
    env: &Env,
    gov_id: &Address,
    proposer: &Address,
    target: &Address,
    function: &Symbol,
    call_args: &Vec<soroban_sdk::Val>,
) {
    let args = soroban_sdk::vec![
        env,
        proposer.into_val(env),
        target.into_val(env),
        function.into_val(env),
        call_args.into_val(env),
    ];
    env.mock_auths(&[MockAuth {
        address: proposer,
        invoke: &MockAuthInvoke {
            contract: gov_id,
            fn_name: "propose",
            args,
            sub_invokes: &[],
        },
    }]);
}

/// A quorum of governance votes gates `set_anchor_rate_limit`; once
/// passed, `execute` applies it with no auth entries of its own.
#[test]
fn set_anchor_rate_limit_requires_full_quorum_then_executes() {
    let (env, gov, anchor, gov_id, m1, m2) = setup();

    let function = Symbol::new(&env, "set_anchor_rate_limit");
    let call_args = Vec::from_array(&env, [1u32.into_val(&env), 3600u32.into_val(&env)]);

    mock_propose_auth(&env, &gov_id, &m1, &anchor.address, &function, &call_args);
    let id = gov.propose(&m1, &anchor.address, &function, &call_args);

    mock_vote_auth(&env, &gov_id, &m1, id, true);
    gov.vote(&m1, &id, &true);

    // Only one of two votes is in: 50% of 100% required quorum.
    env.set_auths(&[]);
    assert!(
        gov.try_execute(&id).is_err(),
        "must not execute below the configured quorum"
    );

    mock_vote_auth(&env, &gov_id, &m2, id, true);
    gov.vote(&m2, &id, &true);

    // `execute` needs no auth entries at all — the governed call's
    // `merchant.require_auth()` is satisfied by the host's own
    // self-authorization rule for the governance contract's address.
    env.set_auths(&[]);
    gov.execute(&id);

    assert_eq!(
        anchor.get_anchor_rate_limit(),
        receipt_anchor::RateLimitConfig {
            burst_capacity: 1,
            refill_interval_secs: 3600,
        }
    );
}

/// `anchor_batch` and `prune_batches` — both merchant-gated — also run only
/// once governance has approved them, end to end.
#[test]
fn anchor_and_prune_run_through_governance() {
    let (env, gov, anchor, gov_id, m1, m2) = setup();
    let root = BytesN::from_array(&env, &[9u8; 32]);

    let anchor_fn = Symbol::new(&env, "anchor_batch");
    let anchor_args = Vec::from_array(
        &env,
        [
            DEFAULT_SHARD.into_val(&env),
            root.into_val(&env),
            10u32.into_val(&env),
            100u64.into_val(&env),
            200u64.into_val(&env),
        ],
    );

    mock_propose_auth(
        &env,
        &gov_id,
        &m1,
        &anchor.address,
        &anchor_fn,
        &anchor_args,
    );
    let anchor_proposal = gov.propose(&m1, &anchor.address, &anchor_fn, &anchor_args);
    mock_vote_auth(&env, &gov_id, &m1, anchor_proposal, true);
    gov.vote(&m1, &anchor_proposal, &true);
    mock_vote_auth(&env, &gov_id, &m2, anchor_proposal, true);
    gov.vote(&m2, &anchor_proposal, &true);
    env.set_auths(&[]);
    gov.execute(&anchor_proposal);

    let batch = anchor.get_batch(&DEFAULT_SHARD, &1);
    assert_eq!(batch.count, 10, "the anchored batch must be stored");

    let prune_fn = Symbol::new(&env, "prune_batches");
    let prune_args = Vec::from_array(
        &env,
        [DEFAULT_SHARD.into_val(&env), 1_000_000u32.into_val(&env)],
    );

    mock_propose_auth(&env, &gov_id, &m2, &anchor.address, &prune_fn, &prune_args);
    let prune_proposal = gov.propose(&m2, &anchor.address, &prune_fn, &prune_args);
    mock_vote_auth(&env, &gov_id, &m1, prune_proposal, true);
    gov.vote(&m1, &prune_proposal, &true);
    mock_vote_auth(&env, &gov_id, &m2, prune_proposal, true);
    gov.vote(&m2, &prune_proposal, &true);
    env.set_auths(&[]);
    gov.execute(&prune_proposal);

    assert!(
        anchor.try_get_batch(&DEFAULT_SHARD, &1).is_err(),
        "the pruned batch must be gone"
    );
}
