#![cfg(test)]

use receipt_anchor::{ReceiptAnchor, ReceiptAnchorClient};
use refund_vault::{RefundVault, RefundVaultClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    vec, Address, Bytes, BytesN, Env,
};

const FLOAT: i128 = 1_000_000;
const WINDOW: u32 = 100;

/// The `ReceiptShard` wasm, built by `cargo build -p receipt-shard --target
/// wasm32v1-none --release` before these tests run (see
/// `.github/workflows/ci.yml` and the README's "Build and test" section).
/// `ReceiptAnchor::anchor_batch` factory-deploys shards from a real installed
/// wasm hash, so this integration test needs the same wasm the unit tests do.
mod shard_wasm {
    soroban_sdk::contractimport!(file = "../../target/wasm32v1-none/release/receipt_shard.wasm");
}

struct TestEnv<'a> {
    env: Env,
    anchor: ReceiptAnchorClient<'a>,
    vault: RefundVaultClient<'a>,
    merchant: Address,
    #[allow(dead_code)]
    token: Address,
}

fn setup<'a>() -> TestEnv<'a> {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token = sac.address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);

    let anchor_id = env.register(ReceiptAnchor, ());
    let anchor = ReceiptAnchorClient::new(&env, &anchor_id);
    let shard_wasm_hash = env.deployer().upload_contract_wasm(shard_wasm::WASM);
    anchor.initialize(&merchant, &shard_wasm_hash);

    let vault_id = env.register(RefundVault, ());
    let vault = RefundVaultClient::new(&env, &vault_id);
    vault.initialize(&merchant, &token, &WINDOW);

    // Initial sequence number
    env.ledger().with_mut(|li| li.sequence_number = 10);

    TestEnv {
        env,
        anchor,
        vault,
        merchant,
        token,
    }
}

// Helper to hash two children (sorted-pair)
fn hash_pair(env: &Env, a: &BytesN<32>, b: &BytesN<32>) -> BytesN<32> {
    let (lo, hi) = if a.to_array() <= b.to_array() {
        (a.to_array(), b.to_array())
    } else {
        (b.to_array(), a.to_array())
    };
    let mut combined = [0u8; 64];
    combined[..32].copy_from_slice(&lo);
    combined[32..].copy_from_slice(&hi);
    let digest = env
        .crypto()
        .sha256(&Bytes::from_slice(env, &combined))
        .to_array();
    BytesN::from_array(env, &digest)
}

// ─────────────────────────────────────────────────────────────────────────────
// README invariant: "Refunds outlive pruned batches"
// -------------------------------------------------------------------------------------
#[test]
fn readme_claim_refunds_outlive_pruned_batches() {
    let TestEnv {
        env,
        anchor,
        vault,
        merchant,
        token: _,
    } = setup();

    // Anchor a single-leaf batch.
    let payment_ref = BytesN::from_array(&env, &[7u8; 32]);
    let leaf = payment_ref.clone();
    anchor.anchor_batch(&leaf.clone(), &1, &0, &100);

    // Fast-forward and prune the batch (anchored at ledger 10, so prune < 150).
    env.ledger().with_mut(|li| li.sequence_number = 200);
    anchor.prune_batches(&150);
    assert!(anchor.try_get_batch(&1).is_err(), "batch should be pruned");

    // The vault is unaffected: refunding the same payment_ref still works,
    // provided it falls within the refund window (paid_at_ledger >= 100 here).
    vault.deposit(&merchant, &500_000);
    let buyer = Address::generate(&env);
    vault.refund(&payment_ref, &buyer, &100, &150, &100, &None);

    let record = vault.get_refund(&payment_ref).unwrap();
    assert_eq!(record.amount_refunded, 100);
}

// ─────────────────────────────────────────────────────────────────────────────
// README invariant: "payment_ref ↔ receipt-leaf"
// -------------------------------------------------------------------------------------
#[test]
fn readme_claim_payment_ref_is_receipt_leaf() {
    let TestEnv {
        env,
        anchor,
        vault,
        merchant,
        token: _,
    } = setup();

    let payment_ref = BytesN::from_array(&env, &[9u8; 32]);
    let leaf = payment_ref.clone(); // The leaf IS the payment_ref.

    let sibling = BytesN::from_array(&env, &[10u8; 32]);
    let root = hash_pair(&env, &leaf, &sibling);

    anchor.anchor_batch(&root, &2, &0, &100);
    let proof = vec![&env, sibling.clone()];
    assert!(anchor.verify_receipt(&1, &leaf, &proof));

    vault.deposit(&merchant, &500_000);
    let buyer = Address::generate(&env);
    vault.refund(&payment_ref, &buyer, &100, &0, &100, &None);

    // Both contracts agree: verify_receipt accepts the leaf and get_refund
    // returns a record keyed by the same bytes.
    assert!(anchor.verify_receipt(&1, &leaf, &proof));
    assert_eq!(vault.get_refund(&payment_ref).unwrap().amount_refunded, 100);
}

// ─────────────────────────────────────────────────────────────────────────────
// Remaining cross-contract behaviour (kept from the previous suite)
// -------------------------------------------------------------------------------------
#[test]
fn test_happy_path_and_payment_ref_correspondence() {
    let TestEnv {
        env,
        anchor,
        vault,
        merchant,
        token: _,
    } = setup();

    let payment_ref = BytesN::from_array(&env, &[7u8; 32]);
    let leaf = payment_ref.clone();

    let sibling = BytesN::from_array(&env, &[8u8; 32]);
    let root = hash_pair(&env, &leaf, &sibling);

    anchor.anchor_batch(&root, &2, &0, &100);
    let proof = vec![&env, sibling.clone()];
    assert!(anchor.verify_receipt(&1, &leaf, &proof));

    vault.deposit(&merchant, &500_000);
    let buyer = Address::generate(&env);
    vault.refund(&payment_ref, &buyer, &100, &0, &100, &None);

    assert!(anchor.verify_receipt(&1, &leaf, &proof));
    assert_eq!(vault.get_refund(&payment_ref).unwrap().amount_refunded, 100);
}

#[test]
fn test_refund_of_payment_in_pruned_batch() {
    let TestEnv {
        env,
        anchor,
        vault,
        merchant,
        token: _,
    } = setup();

    let payment_ref = BytesN::from_array(&env, &[7u8; 32]);
    let leaf = payment_ref.clone();

    let root = leaf.clone();
    anchor.anchor_batch(&root, &1, &0, &100);

    env.ledger().with_mut(|li| li.sequence_number = 200);
    anchor.prune_batches(&150);
    assert!(anchor.try_get_batch(&1).is_err());

    vault.deposit(&merchant, &500_000);
    let buyer = Address::generate(&env);
    vault.refund(&payment_ref, &buyer, &100, &150, &100, &None);

    assert_eq!(vault.get_refund(&payment_ref).unwrap().amount_refunded, 100);
}

#[test]
#[should_panic(expected = "Error(Contract, #19)")]
fn test_full_refund_then_exceed_payment() {
    let TestEnv {
        env,
        anchor,
        vault,
        merchant,
        token: _,
    } = setup();

    let payment_ref = BytesN::from_array(&env, &[7u8; 32]);
    let leaf = payment_ref.clone();

    let root = leaf.clone();
    anchor.anchor_batch(&root, &1, &0, &100);

    vault.deposit(&merchant, &500_000);
    let buyer = Address::generate(&env);

    // First refund takes a partial; a second past the ceiling is rejected.
    vault.refund(&payment_ref, &buyer, &100, &0, &100, &None);
    assert_eq!(vault.get_refund(&payment_ref).unwrap().amount_refunded, 100);

    // Over the ceiling -> Error(Contract, #19) ExceedsPayment.
    vault.refund(&payment_ref, &buyer, &100, &0, &100, &None);
}

#[test]
fn test_pause_interaction() {
    let TestEnv {
        env,
        anchor,
        vault,
        merchant: _,
        token: _,
    } = setup();

    vault.pause();

    let payment_ref = BytesN::from_array(&env, &[7u8; 32]);
    let buyer = Address::generate(&env);
    assert!(vault
        .try_refund(&payment_ref, &buyer, &100, &0, &100, &None)
        .is_err());

    let root = payment_ref.clone();
    anchor.anchor_batch(&root, &1, &0, &100);

    let proof = vec![&env];
    assert!(anchor.verify_receipt(&1, &payment_ref, &proof));
}

#[test]
fn test_ttl_archival_across_both() {
    let TestEnv {
        env,
        anchor,
        vault,
        merchant,
        token: _,
    } = setup();

    let payment_ref = BytesN::from_array(&env, &[7u8; 32]);
    let root = payment_ref.clone();

    anchor.anchor_batch(&root, &1, &0, &100);
    vault.deposit(&merchant, &500_000);

    let buyer = Address::generate(&env);
    vault.refund(&payment_ref, &buyer, &100, &0, &100, &None);

    anchor.extend_batch_ttl(&1);
    vault.extend_refund_ttl(&payment_ref);

    assert!(anchor.verify_receipt(&1, &payment_ref, &vec![&env]));
    assert_eq!(vault.get_refund(&payment_ref).unwrap().amount_refunded, 100);
}
