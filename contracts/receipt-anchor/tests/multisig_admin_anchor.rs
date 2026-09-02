//! #97 — ReceiptAnchor administered by a multisig *contract account*.
//!
//! ReceiptAnchor has the same single-`Address` admin shape as RefundVault.
//! These tests prove that initialising it with a `MultisigAccount` as merchant
//! enforces the account's threshold on the privileged calls (`anchor_batch`,
//! `prune_batches`), with no change to the anchor itself.

use multisig_account::testutils::{make_auth_entry, make_auth_entry_with_nonce};
use multisig_account::{MultisigAccount, MultisigAccountClient};
use receipt_anchor::{ReceiptAnchor, ReceiptAnchorClient};
use soroban_sdk::{testutils::Address as _, vec, Address, BytesN, Env, IntoVal, Val};

/// Logical shard used by these multisig-admin tests.
const DEFAULT_SHARD: u64 = 0;

/// The `ReceiptShard` wasm, built by `cargo build -p receipt-shard --target
/// wasm32v1-none --release` before these tests run (see `.github/workflows/ci.yml`
/// and the README's "Build and test" section). `anchor_batch` factory-deploys
/// shards from a real installed wasm hash.
mod shard_wasm {
    soroban_sdk::contractimport!(file = "../../target/wasm32v1-none/release/receipt_shard.wasm");
}

/// Deploy the multisig account (signers `s1`, `s2`, threshold 2) and a
/// `ReceiptAnchor` initialised with the account as merchant.
///
/// No `mock_all_auths()`: the point of these tests is that the host runs the
/// account's real `__check_auth`.
fn setup() -> (Env, Address, Address, Address, Address) {
    let env = Env::default();

    let s1 = Address::generate(&env);
    let s2 = Address::generate(&env);

    let multisig_id = env.register(MultisigAccount, (vec![&env, s1.clone(), s2.clone()], 2u32));
    let _ = MultisigAccountClient::new(&env, &multisig_id);

    let anchor_id = env.register(ReceiptAnchor, ());
    let anchor = ReceiptAnchorClient::new(&env, &anchor_id);
    // `initialize` is not auth-gated, so it needs no auth entries.
    let shard_wasm_hash = env.deployer().upload_contract_wasm(shard_wasm::WASM);
    anchor.initialize(&multisig_id, &shard_wasm_hash);

    (env, anchor_id, multisig_id, s1, s2)
}

/// `anchor_batch` args as `Val`s, in call order.
fn anchor_batch_args(env: &Env, root: &BytesN<32>) -> Vec<Val> {
    [
        DEFAULT_SHARD.into_val(env),
        root.clone().into_val(env),
        10u32.into_val(env),
        100u64.into_val(env),
        200u64.into_val(env),
    ]
    .to_vec()
}

/// A privileged call (`anchor_batch`) completes when the account's full
/// threshold of signers authorises it, and the batch is actually stored.
#[test]
fn anchor_batch_succeeds_under_multisig_admin() {
    let (env, anchor_id, multisig_id, s1, s2) = setup();
    let root = BytesN::from_array(&env, &[7u8; 32]);

    let args = anchor_batch_args(&env, &root);
    let entry = make_auth_entry(
        &env,
        &multisig_id,
        &anchor_id,
        "anchor_batch",
        &args,
        &[s1, s2],
    );
    env.set_auths(&[entry]);

    let anchor = ReceiptAnchorClient::new(&env, &anchor_id);
    let batch_id = anchor.anchor_batch(&DEFAULT_SHARD, &root, &10, &100, &200);
    let record = anchor.get_batch(&DEFAULT_SHARD, &batch_id);
    assert_eq!(record.count, 10, "the anchored batch must be stored");
}

/// The account's own rule (threshold 2) rejects `anchor_batch` when only one
/// signer attaches — the batch must not be stored.
#[test]
fn anchor_rejects_call_below_multisig_threshold() {
    let (env, anchor_id, multisig_id, s1, _s2) = setup();
    let root = BytesN::from_array(&env, &[7u8; 32]);

    let args = anchor_batch_args(&env, &root);
    let entry = make_auth_entry(&env, &multisig_id, &anchor_id, "anchor_batch", &args, &[s1]);
    env.set_auths(&[entry]);

    let anchor = ReceiptAnchorClient::new(&env, &anchor_id);
    assert!(
        anchor
            .try_anchor_batch(&DEFAULT_SHARD, &root, &10, &100, &200)
            .is_err(),
        "a single signer must not clear a threshold of two"
    );
}

/// `prune_batches`, the other merchant-gated call, also runs under the
/// multisig account.
#[test]
fn prune_batches_succeeds_under_multisig_admin() {
    let (env, anchor_id, multisig_id, s1, s2) = setup();
    let root = BytesN::from_array(&env, &[7u8; 32]);

    // Anchor a batch first (full threshold), then prune under full threshold.
    let args = anchor_batch_args(&env, &root);
    let entry = make_auth_entry(
        &env,
        &multisig_id,
        &anchor_id,
        "anchor_batch",
        &args,
        &[s1.clone(), s2.clone()],
    );
    env.set_auths(&[entry]);
    let anchor = ReceiptAnchorClient::new(&env, &anchor_id);
    let batch_id = anchor.anchor_batch(&DEFAULT_SHARD, &root, &10, &100, &200);

    let prune_args: Vec<Val> = [DEFAULT_SHARD.into_val(&env), 1_000_000u32.into_val(&env)].to_vec();
    // Same account authorised twice in one env requires a fresh nonce.
    let entry = make_auth_entry_with_nonce(
        &env,
        &multisig_id,
        &anchor_id,
        "prune_batches",
        &prune_args,
        &[s1, s2],
        2,
    );
    env.set_auths(&[entry]);
    anchor.prune_batches(&DEFAULT_SHARD, &1_000_000);

    assert!(
        anchor.try_get_batch(&DEFAULT_SHARD, &batch_id).is_err(),
        "the pruned batch must be gone"
    );
}
