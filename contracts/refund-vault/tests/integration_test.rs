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

#[test]
fn test_happy_path_and_payment_ref_correspondence() {
    let TestEnv {
        env,
        anchor,
        vault,
        merchant,
        token: _,
    } = setup();

    // 1. The happy path across both contracts.
    // 2. payment_ref correspondence:
    // The payment_ref used in RefundVault is EXACTLY the leaf hash used in ReceiptAnchor.
    // This explicitly documents the join between the two contracts.
    let payment_ref = BytesN::from_array(&env, &[7u8; 32]);
    let leaf = payment_ref.clone(); // The leaf IS the payment_ref

    let sibling = BytesN::from_array(&env, &[8u8; 32]);
    let root = hash_pair(&env, &leaf, &sibling);

    // Anchor the batch
    anchor.anchor_batch(&root, &2, &0, &100);

    // The agent can verify the receipt
    let proof = vec![&env, sibling.clone()];
    assert!(anchor.verify_receipt(&1, &leaf, &proof));

    // Merchant deposits float
    vault.deposit(&merchant, &500_000);

    // Later, the payment is refunded
    let buyer = Address::generate(&env);
    vault.refund(&payment_ref, &buyer, &100, &0);

    // Assert that anchoring and refunding are independent:
    // The anchored receipt still verifies afterwards.
    assert!(anchor.verify_receipt(&1, &leaf, &proof));
    assert_eq!(vault.get_refund(&payment_ref).unwrap().amount, 100);
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

    // 3. Refund of a payment in a pruned batch.
    // Intended behaviour: Refunds outlive their anchored batch. The vault's records
    // are persistent and independent of the anchor's pruned batches. A refund can
    // arrive and be processed even if the original anchor batch is gone, as long as
    // it satisfies the refund window policy.

    let payment_ref = BytesN::from_array(&env, &[7u8; 32]);
    let leaf = payment_ref.clone();

    let root = leaf.clone(); // Single leaf tree
    anchor.anchor_batch(&root, &1, &0, &100);

    // Fast forward to prune the batch (assume anchored_ledger is 10)
    env.ledger().with_mut(|li| li.sequence_number = 200);
    anchor.prune_batches(&150); // Prunes batches anchored before ledger 150

    // Ensure it's pruned
    assert!(anchor.try_get_batch(&1).is_err());

    // Merchant deposits float
    vault.deposit(&merchant, &500_000);

    // Issue refund - it should succeed even though the batch is pruned,
    // as long as the paid_at_ledger is within the refund window (window is 100).
    // Ledger is 200, window is 100, so paid_at_ledger must be >= 100.
    let buyer = Address::generate(&env);
    vault.refund(&payment_ref, &buyer, &100, &150);

    assert_eq!(vault.get_refund(&payment_ref).unwrap().amount, 100);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_double_refund_with_valid_receipt_proof() {
    let TestEnv {
        env,
        anchor,
        vault,
        merchant,
        token: _,
    } = setup();

    // 4. Double refund with a valid receipt proof.
    // A valid Merkle proof does not create a second refund path.
    // We demonstrate this by trying to refund twice. (The proof itself is off-chain
    // to the vault, the vault only cares about the payment_ref).

    let payment_ref = BytesN::from_array(&env, &[7u8; 32]);
    let leaf = payment_ref.clone();

    let root = leaf.clone();
    anchor.anchor_batch(&root, &1, &0, &100);

    vault.deposit(&merchant, &500_000);
    let buyer = Address::generate(&env);

    // First refund succeeds
    vault.refund(&payment_ref, &buyer, &100, &0);
    assert_eq!(vault.get_refund(&payment_ref).unwrap().amount, 100);

    // Second refund fails with AlreadyRefunded
    vault.refund(&payment_ref, &buyer, &100, &0);
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

    // 5. Pause interaction.
    // A paused vault while anchoring continues; assert anchoring is unaffected.

    // Pause the vault
    vault.pause();

    // Try a vault operation - it should fail
    let payment_ref = BytesN::from_array(&env, &[7u8; 32]);
    let buyer = Address::generate(&env);
    assert!(vault.try_refund(&payment_ref, &buyer, &100, &0).is_err());

    // Anchoring continues unaffected
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

    // 6. TTL/archival across both.
    // extend_batch_ttl and extend_refund_ttl operating on records for the same logical payment.

    let payment_ref = BytesN::from_array(&env, &[7u8; 32]);
    let root = payment_ref.clone();

    anchor.anchor_batch(&root, &1, &0, &100);
    vault.deposit(&merchant, &500_000);

    let buyer = Address::generate(&env);
    vault.refund(&payment_ref, &buyer, &100, &0);

    // Anyone can extend the TTL of both independently
    anchor.extend_batch_ttl(&1);
    vault.extend_refund_ttl(&payment_ref);

    assert!(anchor.verify_receipt(&1, &payment_ref, &vec![&env]));
    assert_eq!(vault.get_refund(&payment_ref).unwrap().amount, 100);
}
