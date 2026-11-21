//! #961 — a `claim_batch` element that fails after a transfer reverts the whole
//! invocation, including the nonce the earlier element consumed.
//!
//! `claim_batch` (`src/lib.rs:1418`) documents "Atomic: a failing element's
//! error reverts the whole invocation, token transfers and events included", and
//! its loop (`:1438-1441`) is a plain `for` over `claim_single`, so nothing in
//! the entry point itself implements the rollback — it is the host reverting the
//! failed contract call. That makes it exactly the kind of guarantee a test can
//! silently stop proving: `src/test.rs::test_claim_batch_partial_failure_reverts_everything`
//! fails its middle element on a *zero ceiling*, which `claim_single` rejects at
//! `settlement::resolve_ceiling` before any transfer, and
//! `test_claim_batch_float_checked_per_item` asserts balances and records but
//! never reads a nonce.
//!
//! So this file takes the float path — where element 1 has already transferred
//! tokens, written its `RefundV2` record, bumped `DataKey::LastClaim`, and
//! advanced the vault's global `Nonce` — and asserts all of it is gone. The
//! global-nonce assertion is the one that can only fail here: element 1's
//! `increment_nonce` really ran, so if the revert did not cover instance storage
//! the counter would read one higher.
//!
//! The caller's replay counter is pinned from the other side: because the failed
//! batch rolled back `DataKey::UserNonce` too, `nonce = 0` is still the value the
//! corrected retry must supply. A `claim_batch` that bumped it before the loop
//! would leave the caller's retry rejected with `StaleState`.
//!
//! Setup uses `refund_window: 0` (and no policy mirrors) deliberately: with the
//! time gate configured, a rejected batch would have an available second cause,
//! and this file is about which element the loop stopped on.
//!
//! As everywhere in this repo, `env.events()` reports only the last top-level
//! invocation, so each event assertion runs immediately after the call whose
//! events it describes and before any counter or record is read back.

use accensa_common::{Error, VaultInit};
use refund_vault::{RefundClaim, RefundVault, RefundVaultClient};
use soroban_sdk::{
    testutils::{Address as _, Events},
    token::{StellarAssetClient, TokenClient},
    vec, Address, BytesN, Env, Vec,
};

/// The vault's float, exactly as the issue specifies it: enough for one 60,000
/// claim and not for two.
const FLOAT: i128 = 100_000;
const CLAIM: i128 = 60_000;

struct Vault {
    env: Env,
    contract: Address,
    client: RefundVaultClient<'static>,
    merchant: Address,
    token: TokenClient<'static>,
}

fn setup() -> Vault {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);

    let contract = env.register(
        RefundVault,
        (VaultInit {
            merchant: merchant.clone(),
            token: token.clone(),
            time_policy: None,
            vdf_policy: None,
            fee_bps: 0,
            fee_recipient: None,
            refund_window: 0,
            deadline: 0,
            vdf_delay: 0,
        },),
    );
    let client = RefundVaultClient::new(&env, &contract);
    client.deposit(&merchant, &FLOAT, &None);

    let token = TokenClient::new(&env, &token);

    Vault {
        env,
        contract,
        client,
        merchant,
        token,
    }
}

fn ref_at(env: &Env, i: u8) -> BytesN<32> {
    BytesN::from_array(env, &[i; 32])
}

fn claim(env: &Env, i: u8, recipient: &Address, amount: i128) -> RefundClaim {
    RefundClaim {
        payment_ref: ref_at(env, i),
        recipient: recipient.clone(),
        amount,
        paid_at_ledger: 0,
        // Ceiling == amount, per the issue: the only thing that can reject these
        // elements is the float.
        payment_amount: amount,
        vdf_proof: None,
    }
}

/// The two 60,000 claims whose second element cannot be paid.
fn over_draft_batch(env: &Env, buyer_a: &Address, buyer_b: &Address) -> Vec<RefundClaim> {
    vec![
        env,
        claim(env, 1, buyer_a, CLAIM),
        claim(env, 2, buyer_b, CLAIM),
    ]
}

/// The vault publishes nothing for a rolled-back invocation.
fn assert_no_events(v: &Vault) {
    assert_eq!(
        v.env.events().all().filter_by_contract(&v.contract),
        vec![&v.env],
        "a reverted batch must not publish the elements it did run",
    );
}

/// No token moved, in either direction, for a batch that reverted.
fn assert_nothing_paid(v: &Vault, buyers: &[&Address]) {
    assert_eq!(v.token.balance(&v.contract), FLOAT);
    for buyer in buyers {
        assert_eq!(v.token.balance(buyer), 0);
    }
}

/// The batch answers `InsufficientFloat` and pays nobody — the second element's
/// failure erases the first element's transfer.
#[test]
fn the_second_element_failing_on_float_reverts_the_first_elements_transfer() {
    let v = setup();
    let buyer_a = Address::generate(&v.env);
    let buyer_b = Address::generate(&v.env);
    let claims = over_draft_batch(&v.env, &buyer_a, &buyer_b);

    assert_eq!(
        v.client.try_claim_batch(&claims, &0),
        Err(Ok(Error::InsufficientFloat)),
    );
    assert_no_events(&v);
    // Element 1 had already transferred 60,000 to `buyer_a` inside the call.
    assert_nothing_paid(&v, &[&buyer_a, &buyer_b]);
}

/// Neither element's `RefundV2` record survives, so the reverted payment ref is
/// refundable again rather than half-written.
#[test]
fn neither_payment_ref_keeps_a_record() {
    let v = setup();
    let buyer_a = Address::generate(&v.env);
    let buyer_b = Address::generate(&v.env);
    let claims = over_draft_batch(&v.env, &buyer_a, &buyer_b);

    assert_eq!(
        v.client.try_claim_batch(&claims, &0),
        Err(Ok(Error::InsufficientFloat)),
    );

    assert!(v.client.get_refund(&ref_at(&v.env, 1)).is_none());
    assert!(v.client.get_refund(&ref_at(&v.env, 2)).is_none());
}

/// Both counters are untouched. The vault's global `Nonce` is the load-bearing
/// half: element 1 reached `increment_nonce`, so a revert that missed instance
/// storage would leave it one higher and shift every later event's `nonce` field.
#[test]
fn the_reverted_batch_consumes_neither_nonce() {
    let v = setup();
    let buyer_a = Address::generate(&v.env);
    let buyer_b = Address::generate(&v.env);
    let claims = over_draft_batch(&v.env, &buyer_a, &buyer_b);

    // `deposit` shares the global counter with claims, so the baseline is measured
    // rather than assumed. The caller's replay counter is separate and untouched
    // by a deposit, which is why a funded vault still expects `nonce = 0`.
    let vault_nonce = v.client.get_nonce();
    assert_eq!(v.client.get_user_nonce(&v.merchant), 0);

    assert_eq!(
        v.client.try_claim_batch(&claims, &0),
        Err(Ok(Error::InsufficientFloat)),
    );

    assert_eq!(
        v.client.get_nonce(),
        vault_nonce,
        "element 1 bumped the vault nonce; the revert must undo it",
    );
    assert_eq!(
        v.client.get_user_nonce(&v.merchant),
        0,
        "a rejected batch must leave the caller's nonce available for its retry",
    );
}

/// The retry with the *same* nonce succeeds once the batch no longer overdrafts,
/// which is what a caller needs: a rejected `claim_batch` costs nothing to redo.
#[test]
fn the_same_nonce_pays_after_the_batch_is_corrected() {
    let v = setup();
    let buyer_a = Address::generate(&v.env);
    let buyer_b = Address::generate(&v.env);
    let claims = over_draft_batch(&v.env, &buyer_a, &buyer_b);

    assert_eq!(
        v.client.try_claim_batch(&claims, &0),
        Err(Ok(Error::InsufficientFloat)),
    );

    // Drop the element the float cannot cover and replay with the same nonce.
    let corrected = vec![&v.env, claim(&v.env, 1, &buyer_a, CLAIM)];
    v.client.claim_batch(&corrected, &0);

    assert_eq!(v.token.balance(&buyer_a), CLAIM);
    assert_eq!(v.token.balance(&v.contract), FLOAT - CLAIM);
    assert_eq!(
        v.client
            .get_refund(&ref_at(&v.env, 1))
            .unwrap()
            .amount_refunded,
        CLAIM
    );
    assert_eq!(v.client.get_user_nonce(&v.merchant), 1);

    // And the *next* nonce is the one the reverted batch would have consumed had
    // it bumped before the loop. 40,000 of float is left after the retry above.
    let second = vec![&v.env, claim(&v.env, 2, &buyer_b, 40_000)];
    assert_eq!(
        v.client.try_claim_batch(&second, &0),
        Err(Ok(Error::StaleState)),
    );
    v.client.claim_batch(&second, &1);
    assert_eq!(v.token.balance(&buyer_b), 40_000);
    assert_eq!(v.token.balance(&v.contract), 0);
    assert_eq!(v.client.get_user_nonce(&v.merchant), 2);
}

/// Reverting is per-invocation, not per-vault: a batch that fails does not
/// disarm the payment refs it touched, and each element is still refundable
/// afterwards on its own.
#[test]
fn a_reverted_batch_leaves_its_refs_reusable() {
    let v = setup();
    let buyer_a = Address::generate(&v.env);
    let buyer_b = Address::generate(&v.env);
    let claims = over_draft_batch(&v.env, &buyer_a, &buyer_b);

    assert_eq!(
        v.client.try_claim_batch(&claims, &0),
        Err(Ok(Error::InsufficientFloat)),
    );

    // Both refs paid singly, in two calls, against the same 100,000 float.
    v.client
        .claim_batch(&vec![&v.env, claim(&v.env, 2, &buyer_b, CLAIM)], &0);
    v.client
        .claim_batch(&vec![&v.env, claim(&v.env, 1, &buyer_a, 40_000)], &1);

    assert_eq!(v.token.balance(&buyer_b), CLAIM);
    assert_eq!(v.token.balance(&buyer_a), 40_000);
    assert_eq!(
        v.client
            .get_refund(&ref_at(&v.env, 1))
            .unwrap()
            .amount_refunded,
        40_000
    );
    assert_eq!(v.token.balance(&v.contract), 0);
}
