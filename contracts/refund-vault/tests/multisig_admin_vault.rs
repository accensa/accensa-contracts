//! #97 — RefundVault administered by a multisig *contract account*.
//!
//! On Soroban an `Address` is not necessarily a keypair: it can be a contract
//! that implements `__check_auth`, and `require_auth()` delegates to that
//! implementation. These tests prove that a `RefundVault` initialised with a
//! `MultisigAccount` as its merchant enforces the account's threshold on
//! privileged calls — with no threshold logic in the vault itself.
//!
//! The auth entries are built by hand with [`multisig_account::testutils`],
//! mirroring what wallet tooling produces for a contract account with
//! delegated signers.

use accensa_common::{Error, VaultInit};
use multisig_account::testutils::{make_auth_entry_no_args, make_auth_entry_with_nonce};
use multisig_account::{MultisigAccount, MultisigAccountClient};
use refund_vault::coupons::{CouponRecord, MAX_COUPON_DISCOUNT_BPS};
use refund_vault::{DataKey, RefundVault, RefundVaultClient};
use soroban_sdk::{testutils::Address as _, vec, Address, Env, IntoVal, Val};

/// Deploy the multisig account (signers `s1`, `s2`, threshold 2) and a
/// `RefundVault` initialised with the account as merchant.
///
/// No `mock_all_auths()`: the point of these tests is that the host runs the
/// account's real `__check_auth`.
fn setup() -> (Env, Address, Address, Address, Address) {
    let env = Env::default();

    let s1 = Address::generate(&env);
    let s2 = Address::generate(&env);

    let multisig_id = env.register(MultisigAccount, (vec![&env, s1.clone(), s2.clone()], 2u32));
    let _ = MultisigAccountClient::new(&env, &multisig_id);

    let vault_id = env.register(
        RefundVault,
        ({
            // `initialize` is not auth-gated, so no auth entries are needed. No
            // policy contracts are wired: the calls these tests exercise (`pause`)
            // never touch the refund policy path.
            VaultInit {
                merchant: multisig_id.clone(),
                token: Address::generate(&env),
                time_policy: None,
                vdf_policy: None,
                fee_bps: 0,
                fee_recipient: None,
                refund_window: 100,
                deadline: 0,
                vdf_delay: 0,
            }
        },),
    );

    (env, vault_id, multisig_id, s1, s2)
}

/// A privileged call (`pause`) completes when the account's full threshold of
/// signers authorises it.
#[test]
fn vault_privileged_call_succeeds_under_multisig_admin() {
    let (env, vault_id, multisig_id, s1, s2) = setup();

    let entry = make_auth_entry_no_args(&env, &multisig_id, &vault_id, "pause", &[s1, s2]);
    env.set_auths(&[entry]);

    let vault = RefundVaultClient::new(&env, &vault_id);
    // Returns `()` — panics if the account's `__check_auth` rejects the call.
    vault.pause();
}

/// The account's own rule (threshold 2) rejects a call carrying only one
/// signer, even though every attached signer is legitimate.
#[test]
fn vault_rejects_call_below_multisig_threshold() {
    let (env, vault_id, multisig_id, s1, _s2) = setup();

    let entry = make_auth_entry_no_args(&env, &multisig_id, &vault_id, "pause", &[s1]);
    env.set_auths(&[entry]);

    let vault = RefundVaultClient::new(&env, &vault_id);
    assert!(
        vault.try_pause().is_err(),
        "a single signer must not clear a threshold of two"
    );
}

/// A signer that is not registered on the account is rejected even when the
/// count would otherwise meet the threshold.
#[test]
fn vault_rejects_unknown_signer() {
    let (env, vault_id, multisig_id, s1, s2) = setup();
    let stranger = Address::generate(&env);

    let entry =
        make_auth_entry_no_args(&env, &multisig_id, &vault_id, "pause", &[s1, s2, stranger]);
    env.set_auths(&[entry]);

    let vault = RefundVaultClient::new(&env, &vault_id);
    assert!(
        vault.try_pause().is_err(),
        "an unregistered signer must be rejected"
    );
}

// ── `get_coupon`, `set_settlement_contract`, `get_claim_cooldown` (issue #710)
//
// These three vault entry points had no test at all when the issue was opened,
// and they are admin-facing configuration a multisig-operated merchant actually
// uses. They are driven here through the same real `__check_auth` harness the
// rest of this file uses, so the threshold behaviour of the setters is pinned
// together with the values the getters read back.

/// Authorize `fn_name` on the vault as the multisig account, for a call that
/// carries exactly `args`, with `signers` as the attached approvers.
///
/// Two rules of the test host shape the arguments: the entry has to name the
/// same argument list as the client call — a mismatch is rejected as
/// `Error(Auth, InvalidAction)` before the vault runs — and an account + nonce
/// pair can only be spent once per environment, so `nonce` increases with every
/// authorized call inside a test.
fn authorize(
    env: &Env,
    multisig_id: &Address,
    vault_id: &Address,
    fn_name: &str,
    args: &[Val],
    signers: &[Address],
    nonce: i64,
) {
    env.set_auths(&[make_auth_entry_with_nonce(
        env,
        multisig_id,
        vault_id,
        fn_name,
        args,
        signers,
        nonce,
    )]);
}

/// Read the instance-storage entry `set_settlement_contract` writes. The vault
/// publishes no getter for that key, so the test reads the key the contract
/// itself writes.
fn stored_settlement_contract(env: &Env, vault_id: &Address) -> Option<Address> {
    let key = DataKey::SettlementContract;
    env.as_contract(vault_id, || {
        let value: Option<Address> = env.storage().instance().get(&key);
        value
    })
}

/// A full quorum records the settlement contract, and a later call replaces the
/// stored value rather than refusing because one is already there.
#[test]
fn set_settlement_contract_writes_and_then_overwrites_the_key() {
    let (env, vault_id, multisig_id, s1, s2) = setup();
    let vault = RefundVaultClient::new(&env, &vault_id);
    let first = Address::generate(&env);
    let second = Address::generate(&env);

    assert_eq!(stored_settlement_contract(&env, &vault_id), None);

    authorize(
        &env,
        &multisig_id,
        &vault_id,
        "set_settlement_contract",
        &[first.clone().into_val(&env)],
        &[s1.clone(), s2.clone()],
        1,
    );
    vault.set_settlement_contract(&first);
    assert_eq!(
        stored_settlement_contract(&env, &vault_id),
        Some(first.clone())
    );

    authorize(
        &env,
        &multisig_id,
        &vault_id,
        "set_settlement_contract",
        &[second.clone().into_val(&env)],
        &[s1, s2],
        2,
    );
    vault.set_settlement_contract(&second);
    assert_eq!(stored_settlement_contract(&env, &vault_id), Some(second));
}

/// Below the threshold the key is untouched — the setter's write happens after
/// `require_auth`, so a partial quorum cannot reconfigure settlement.
#[test]
fn set_settlement_contract_is_refused_below_the_multisig_threshold() {
    let (env, vault_id, multisig_id, s1, _s2) = setup();
    let vault = RefundVaultClient::new(&env, &vault_id);
    let settlement = Address::generate(&env);

    authorize(
        &env,
        &multisig_id,
        &vault_id,
        "set_settlement_contract",
        &[settlement.clone().into_val(&env)],
        &[s1],
        1,
    );
    let outcome = vault.try_set_settlement_contract(&settlement);
    assert!(
        matches!(outcome, Err(Err(_))),
        "a single signer must not be able to set the settlement contract, \
         and the rejection must come from the host's auth check rather than \
         one of the vault's own Error arms: {outcome:?}"
    );
    assert_eq!(stored_settlement_contract(&env, &vault_id), None);
}

/// With no `Admin` in instance storage the setter reports `NotInitialized` on its
/// own pre-check instead of falling into `require_auth`.
#[test]
fn set_settlement_contract_reports_not_initialized_without_an_admin() {
    let (env, vault_id, _multisig_id, _s1, _s2) = setup();
    let vault = RefundVaultClient::new(&env, &vault_id);

    env.as_contract(&vault_id, || {
        env.storage().instance().remove(&DataKey::Admin);
    });

    assert_eq!(
        vault.try_set_settlement_contract(&Address::generate(&env)),
        Err(Ok(Error::NotInitialized))
    );
    assert_eq!(stored_settlement_contract(&env, &vault_id), None);
}

/// `get_claim_cooldown` reports the configured interval, and `0` — the value it
/// returns before any admin call — is also the value that disables the gate.
#[test]
fn get_claim_cooldown_tracks_the_multisig_configured_value() {
    let (env, vault_id, multisig_id, s1, s2) = setup();
    let vault = RefundVaultClient::new(&env, &vault_id);

    assert_eq!(vault.get_claim_cooldown(), 0);

    authorize(
        &env,
        &multisig_id,
        &vault_id,
        "set_claim_cooldown",
        &[300u64.into_val(&env)],
        &[s1.clone(), s2.clone()],
        1,
    );
    vault.set_claim_cooldown(&300u64);
    assert_eq!(vault.get_claim_cooldown(), 300);

    authorize(
        &env,
        &multisig_id,
        &vault_id,
        "set_claim_cooldown",
        &[0u64.into_val(&env)],
        &[s1, s2],
        2,
    );
    vault.set_claim_cooldown(&0u64);
    assert_eq!(vault.get_claim_cooldown(), 0);
}

/// A partial quorum cannot switch the vault-wide claim cooldown on.
#[test]
fn set_claim_cooldown_is_refused_below_the_multisig_threshold() {
    let (env, vault_id, multisig_id, s1, _s2) = setup();
    let vault = RefundVaultClient::new(&env, &vault_id);

    authorize(
        &env,
        &multisig_id,
        &vault_id,
        "set_claim_cooldown",
        &[300u64.into_val(&env)],
        &[s1],
        1,
    );
    let outcome = vault.try_set_claim_cooldown(&300u64);
    assert!(
        matches!(outcome, Err(Err(_))),
        "a single signer must not be able to configure the claim cooldown, \
         and the rejection must come from the host's auth check rather than \
         one of the vault's own Error arms: {outcome:?}"
    );
    assert_eq!(vault.get_claim_cooldown(), 0);
}

/// `get_coupon` returns the exact record that was minted, per coupon id, and
/// `None` for an id that was never minted.
#[test]
fn get_coupon_returns_the_minted_record_and_none_for_unknown_ids() {
    let (env, vault_id, multisig_id, s1, s2) = setup();
    let vault = RefundVaultClient::new(&env, &vault_id);
    let holder = Address::generate(&env);
    let other_holder = Address::generate(&env);

    assert_eq!(vault.get_coupon(&7u64), None);

    authorize(
        &env,
        &multisig_id,
        &vault_id,
        "mint_coupon",
        &[
            7u64.into_val(&env),
            holder.clone().into_val(&env),
            1_000u32.into_val(&env),
        ],
        &[s1.clone(), s2.clone()],
        1,
    );
    vault.mint_coupon(&7u64, &holder, &1_000u32);
    assert_eq!(
        vault.get_coupon(&7u64),
        Some(CouponRecord {
            owner: holder.clone(),
            discount_bps: 1_000,
            redeemed: false,
        })
    );

    // Ids are independent slots: minting 8 does not disturb 7, and 9 stays
    // unread.
    authorize(
        &env,
        &multisig_id,
        &vault_id,
        "mint_coupon",
        &[
            8u64.into_val(&env),
            other_holder.clone().into_val(&env),
            2_500u32.into_val(&env),
        ],
        &[s1, s2],
        2,
    );
    vault.mint_coupon(&8u64, &other_holder, &2_500u32);
    assert_eq!(
        vault.get_coupon(&7u64),
        Some(CouponRecord {
            owner: holder,
            discount_bps: 1_000,
            redeemed: false,
        })
    );
    assert_eq!(
        vault.get_coupon(&8u64),
        Some(CouponRecord {
            owner: other_holder,
            discount_bps: 2_500,
            redeemed: false,
        })
    );
    assert_eq!(vault.get_coupon(&9u64), None);
}

/// A rejected mint leaves the record `get_coupon` reads unchanged, and never
/// creates one for the id it refused.
#[test]
fn get_coupon_still_reads_the_record_after_a_rejected_re_mint() {
    let (env, vault_id, multisig_id, s1, s2) = setup();
    let vault = RefundVaultClient::new(&env, &vault_id);
    let holder = Address::generate(&env);

    authorize(
        &env,
        &multisig_id,
        &vault_id,
        "mint_coupon",
        &[
            3u64.into_val(&env),
            holder.clone().into_val(&env),
            1_000u32.into_val(&env),
        ],
        &[s1.clone(), s2.clone()],
        1,
    );
    vault.mint_coupon(&3u64, &holder, &1_000u32);

    // Re-minting a live id is refused and must not rewrite the discount.
    authorize(
        &env,
        &multisig_id,
        &vault_id,
        "mint_coupon",
        &[
            3u64.into_val(&env),
            holder.clone().into_val(&env),
            2_000u32.into_val(&env),
        ],
        &[s1.clone(), s2.clone()],
        2,
    );
    assert_eq!(
        vault.try_mint_coupon(&3u64, &holder, &2_000u32),
        Err(Ok(Error::AlreadyInitialized))
    );
    assert_eq!(
        vault.get_coupon(&3u64),
        Some(CouponRecord {
            owner: holder.clone(),
            discount_bps: 1_000,
            redeemed: false,
        })
    );

    // A discount over the cap is refused before anything is stored.
    authorize(
        &env,
        &multisig_id,
        &vault_id,
        "mint_coupon",
        &[
            4u64.into_val(&env),
            holder.clone().into_val(&env),
            (MAX_COUPON_DISCOUNT_BPS + 1).into_val(&env),
        ],
        &[s1, s2],
        3,
    );
    assert_eq!(
        vault.try_mint_coupon(&4u64, &holder, &(MAX_COUPON_DISCOUNT_BPS + 1)),
        Err(Ok(Error::InvalidRatio))
    );
    assert_eq!(vault.get_coupon(&4u64), None);
}

/// The `None` `get_coupon` returns for an unknown id is exactly what drives
/// `deposit` into its `CouponNotFound` arm.
#[test]
fn deposit_with_an_unknown_coupon_id_reports_coupon_not_found() {
    let (env, vault_id, multisig_id, s1, s2) = setup();
    let vault = RefundVaultClient::new(&env, &vault_id);

    // The coupon lookup happens before any token transfer, so this path needs no
    // funded settlement token.
    authorize(
        &env,
        &multisig_id,
        &vault_id,
        "deposit",
        &[
            multisig_id.clone().into_val(&env),
            1_000i128.into_val(&env),
            Some(99u64).into_val(&env),
        ],
        &[s1.clone(), s2.clone()],
        1,
    );
    assert_eq!(
        vault.try_deposit(&multisig_id, &1_000i128, &Some(99u64)),
        Err(Ok(Error::CouponNotFound))
    );
    assert_eq!(vault.get_coupon(&99u64), None);
}
